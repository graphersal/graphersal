//! `impl GraphStorage for LayeredGraph`: the reads answer from the base (an untouched element
//! with one bit test, no lookup) merged with the delta; every write runs its write rule
//! (`graph/write_rules.rs`) first and then stores in the delta. Writes to base elements and
//! edges touching base vertices are not implemented yet.

use compact_str::CompactString;

use super::handle::{LayeredEdge, LayeredVertex};
use super::{LayeredGraph, delta::VertexView};
use crate::error::GraphError;
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::schema::defaults::CreateContext;
use crate::graph::schema::{ElementKind, GraphSchema, PropertySchema};
use crate::graph::write_rules::{self, PathRemoval};
use crate::graph::{
    CompressionEstimate, CompressionEstimateRequest, CompressionStats, ExecutionPolicy,
    GraphNodeType, GraphStatistics, GraphStorage, PropertyIndexInfo, PropertyIndexRange,
    PropertyIndexTransform, StorageCapabilities, UniqueIndexDuplicates,
};
use crate::value::{ElementProperty, JsonPath, PropertyMap};

/// The operation a write to a base element names until the layer can change base elements
/// (tasks S1.5, S1.6).
const BASE_WRITES: &str = "writes to base elements of a layered graph yet";

/// The operation an edge touching a base vertex names until task S1.4.
const CROSS_EDGES: &str = "edges touching base vertices of a layered graph yet";

/// `Ok` for a write to an element of the delta, [`GraphError::Unsupported`] for one to a base
/// element. Called after the write rule passed and found something to change, so a stale
/// handle, an invalid value or a no-op answers exactly as in `TraversalGraph`.
#[inline]
fn delta_only(is_delta: bool) -> Result<(), GraphError> {
    if is_delta {
        Ok(())
    } else {
        Err(GraphError::unsupported(BASE_WRITES))
    }
}

/// The labels of a vertex view (empty for none).
#[inline]
fn view_labels<'a>(view: Option<VertexView<'a>>) -> &'a [CompactString] {
    view.map(|v| v.labels).unwrap_or(&[])
}

impl GraphStorage for LayeredGraph {
    type VertexId = LayeredVertex;
    type EdgeId = LayeredEdge;

    #[inline]
    fn vertex_count(&self) -> Result<usize, GraphError> {
        Ok(self
            .base
            .graph
            .node_count()
            .saturating_add_signed(self.delta.vertex_count_change()))
    }

    #[inline]
    fn edge_count(&self) -> Result<usize, GraphError> {
        Ok(self
            .base
            .graph
            .edge_count()
            .saturating_add_signed(self.delta.edge_count_change()))
    }

    #[inline]
    fn vertices(&self) -> Result<impl Iterator<Item = LayeredVertex> + '_, GraphError> {
        Ok(GraphStorage::vertices(&*self.base)?
            .filter(self.base_vertex_filter())
            .map(LayeredVertex::from_base)
            .chain(self.delta.vertices()))
    }

    #[inline]
    fn edges(&self) -> Result<impl Iterator<Item = LayeredEdge> + '_, GraphError> {
        Ok(GraphStorage::edges(&*self.base)?
            .filter(self.base_edge_filter())
            .map(LayeredEdge::from_base)
            .chain(self.delta.edges()))
    }

    #[inline]
    fn get_vertex_by_id(&self, id: &str) -> Result<Option<LayeredVertex>, GraphError> {
        if let Some(node) = GraphStorage::get_vertex_by_id(&*self.base, id)?
            && self.base_vertex_live(node)
        {
            return Ok(Some(LayeredVertex::from_base(node)));
        }
        Ok(self.delta.vertex_by_id(id))
    }

    #[inline]
    fn get_edge_by_id(&self, id: &str) -> Result<Option<LayeredEdge>, GraphError> {
        if let Some(edge) = GraphStorage::get_edge_by_id(&*self.base, id)?
            && self.base_edge_live(edge)
        {
            return Ok(Some(LayeredEdge::from_base(edge)));
        }
        Ok(self.delta.edge_by_id(id))
    }

    /// The base's list (a delta vertex has none there: its slot is out of the base's range),
    /// without removed base edges (tested only for a dirty vertex), then the edges the layer
    /// added at `node`.
    #[inline]
    fn out_edges(
        &self,
        node: LayeredVertex,
    ) -> Result<impl Iterator<Item = LayeredEdge> + '_, GraphError> {
        Ok(GraphStorage::out_edges(&*self.base, node.to_base())?
            .filter(self.base_adjacency_filter(node))
            .map(LayeredEdge::from_base)
            .chain(self.delta.out_edges(node)))
    }

    #[inline]
    fn in_edges(
        &self,
        node: LayeredVertex,
    ) -> Result<impl Iterator<Item = LayeredEdge> + '_, GraphError> {
        Ok(GraphStorage::in_edges(&*self.base, node.to_base())?
            .filter(self.base_adjacency_filter(node))
            .map(LayeredEdge::from_base)
            .chain(self.delta.in_edges(node)))
    }

    #[inline]
    fn vertex_id(&self, node: LayeredVertex) -> Result<Option<&str>, GraphError> {
        self.vertex_read(
            node,
            |base, node| GraphStorage::vertex_id(base, node),
            |view| Ok(view.map(|v| v.id)),
        )
    }

    #[inline]
    fn edge_id(&self, edge: LayeredEdge) -> Result<Option<&str>, GraphError> {
        self.edge_read(
            edge,
            |base, edge| GraphStorage::edge_id(base, edge),
            |view| Ok(view.map(|e| e.id)),
        )
    }

    #[inline]
    fn vertex_label(&self, node: LayeredVertex) -> Result<Option<&str>, GraphError> {
        self.vertex_read(
            node,
            |base, node| GraphStorage::vertex_label(base, node),
            |view| Ok(view_labels(view).first().map(|l| l.as_str())),
        )
    }

    #[inline]
    fn edge_label(&self, edge: LayeredEdge) -> Result<Option<&str>, GraphError> {
        self.edge_read(
            edge,
            |base, edge| GraphStorage::edge_label(base, edge),
            |view| Ok(view.and_then(|e| e.label.first()).map(|l| l.as_str())),
        )
    }

    #[inline]
    fn vertex_labels(&self, node: LayeredVertex) -> Result<&[CompactString], GraphError> {
        self.vertex_read(
            node,
            |base, node| GraphStorage::vertex_labels(base, node),
            |view| Ok(view_labels(view)),
        )
    }

    #[inline]
    fn vertex_has_label(&self, node: LayeredVertex, label: &str) -> Result<bool, GraphError> {
        Ok(self
            .vertex_labels(node)?
            .iter()
            .any(|l| l.as_str() == label))
    }

    #[inline]
    fn vertex_properties(&self, node: LayeredVertex) -> Result<Option<&PropertyMap>, GraphError> {
        self.vertex_read(
            node,
            |base, node| GraphStorage::vertex_properties(base, node),
            |view| Ok(view.map(|v| v.properties)),
        )
    }

    #[inline]
    fn edge_properties(&self, edge: LayeredEdge) -> Result<Option<&PropertyMap>, GraphError> {
        self.edge_read(
            edge,
            |base, edge| GraphStorage::edge_properties(base, edge),
            |view| Ok(view.and_then(|e| e.properties)),
        )
    }

    #[inline]
    fn edge_endpoints(
        &self,
        edge: LayeredEdge,
    ) -> Result<Option<(LayeredVertex, LayeredVertex)>, GraphError> {
        self.edge_read(
            edge,
            |base, edge| {
                Ok(GraphStorage::edge_endpoints(base, edge)?.map(|(from, to)| {
                    (LayeredVertex::from_base(from), LayeredVertex::from_base(to))
                }))
            },
            |view| Ok(view.map(|e| e.endpoints)),
        )
    }

    #[inline]
    fn contains_vertex(&self, node: LayeredVertex) -> Result<bool, GraphError> {
        self.vertex_read(
            node,
            |base, node| GraphStorage::contains_vertex(base, node),
            |view| Ok(view.is_some()),
        )
    }

    #[inline]
    fn contains_edge(&self, edge: LayeredEdge) -> Result<bool, GraphError> {
        self.edge_read(
            edge,
            |base, edge| GraphStorage::contains_edge(base, edge),
            |view| Ok(view.is_some()),
        )
    }

    /// The base's label index (no scan), without removed base vertices, then the layer's.
    fn vertices_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Result<Box<dyn Iterator<Item = LayeredVertex> + 'a>, GraphError> {
        Ok(Box::new(
            self.base
                .index
                .get_nodes_by_label(label)
                .into_iter()
                .flatten()
                .filter(self.base_vertex_filter())
                .map(LayeredVertex::from_base)
                .chain(self.delta.vertices_by_label(label)),
        ))
    }

    /// The edge counterpart of [`Self::vertices_by_label`].
    fn edges_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Result<Box<dyn Iterator<Item = LayeredEdge> + 'a>, GraphError> {
        Ok(Box::new(
            self.base
                .index
                .get_edges_by_label(label)
                .into_iter()
                .flatten()
                .filter(self.base_edge_filter())
                .map(LayeredEdge::from_base)
                .chain(self.delta.edges_by_label(label)),
        ))
    }

    /// The layer's own statistics: the base's at creation, kept exact by every write.
    #[inline]
    fn statistics(&self) -> Result<Option<&GraphStatistics>, GraphError> {
        Ok(Some(&self.statistics))
    }

    /// The base's schema.
    fn get_schema(&self) -> Result<Option<GraphSchema>, GraphError> {
        GraphStorage::get_schema(&*self.base)
    }

    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Result<Option<PropertySchema>, GraphError> {
        GraphStorage::declared_property_schema(&*self.base, kind, labels, key)
    }

    /// The base's catalog.
    fn definitions(&self) -> Result<impl Iterator<Item = &Definition> + '_, GraphError> {
        GraphStorage::definitions(&*self.base)
    }

    fn definition(
        &self,
        kind: DefinitionKind,
        name: &str,
    ) -> Result<Option<&Definition>, GraphError> {
        GraphStorage::definition(&*self.base, kind, name)
    }

    /// The base's figures: the layer stores no values of its own yet.
    fn compression_stats(&self, name: &str) -> Result<Option<CompressionStats>, GraphError> {
        GraphStorage::compression_stats(&*self.base, name)
    }

    fn estimate_compression(
        &self,
        request: &CompressionEstimateRequest,
    ) -> Result<CompressionEstimate, GraphError> {
        GraphStorage::estimate_compression(&*self.base, request)
    }

    /// The base's indexes: they cover every element of the layer while nothing is changed.
    fn property_indexes(&self) -> Result<Vec<PropertyIndexInfo>, GraphError> {
        GraphStorage::property_indexes(&*self.base)
    }

    fn vertices_by_property_index<'a>(
        &'a self,
        label: Option<&'a str>,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = LayeredVertex> + 'a>>, GraphError> {
        Ok(
            GraphStorage::vertices_by_property_index(&*self.base, label, key, range)?.map(
                |found| -> Box<dyn Iterator<Item = LayeredVertex> + 'a> {
                    Box::new(found.map(LayeredVertex::from_base))
                },
            ),
        )
    }

    fn edges_by_property_index<'a>(
        &'a self,
        label: Option<&'a str>,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = LayeredEdge> + 'a>>, GraphError> {
        Ok(
            GraphStorage::edges_by_property_index(&*self.base, label, key, range)?.map(
                |found| -> Box<dyn Iterator<Item = LayeredEdge> + 'a> {
                    Box::new(found.map(LayeredEdge::from_base))
                },
            ),
        )
    }

    fn vertices_by_transformed_property_index<'a>(
        &'a self,
        label: Option<&'a str>,
        key: &'a str,
        transform: &PropertyIndexTransform,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = LayeredVertex> + 'a>>, GraphError> {
        Ok(GraphStorage::vertices_by_transformed_property_index(
            &*self.base,
            label,
            key,
            transform,
            range,
        )?
        .map(|found| -> Box<dyn Iterator<Item = LayeredVertex> + 'a> {
            Box::new(found.map(LayeredVertex::from_base))
        }))
    }

    fn edges_by_transformed_property_index<'a>(
        &'a self,
        label: Option<&'a str>,
        key: &'a str,
        transform: &PropertyIndexTransform,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = LayeredEdge> + 'a>>, GraphError> {
        Ok(GraphStorage::edges_by_transformed_property_index(
            &*self.base,
            label,
            key,
            transform,
            range,
        )?
        .map(|found| -> Box<dyn Iterator<Item = LayeredEdge> + 'a> {
            Box::new(found.map(LayeredEdge::from_base))
        }))
    }

    fn count_by_property_index(
        &self,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Result<Option<u64>, GraphError> {
        GraphStorage::count_by_property_index(&*self.base, element, label, key, transform, range)
    }

    fn unique_index_duplicates(
        &self,
        name: &str,
    ) -> Result<Option<UniqueIndexDuplicates>, GraphError> {
        GraphStorage::unique_index_duplicates(&*self.base, name)
    }

    /// The base's policy: a layer runs under the limits its base was built with.
    fn execution_policy(&self) -> Result<ExecutionPolicy, GraphError> {
        GraphStorage::execution_policy(&*self.base)
    }

    /// `writable` (new elements, see the module docs), not `atomic`: the unit methods are the
    /// trait's no-ops until task S1.8, so nothing is rolled back. The property index lookups
    /// are the base's, so they are exact when the base's are.
    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        let base = GraphStorage::capabilities(&*self.base)?;
        Ok(StorageCapabilities::none()
            .with_writable(true)
            .with_exact_property_index_lookups(base.exact_property_index_lookups))
    }

    /// The write rules of `insert_vertex`, then the id (unique across base and delta, the
    /// auto id continuing the base's sequence), then the new vertex is stored in the delta.
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<LayeredVertex, GraphError> {
        let (labels, properties) = write_rules::insert_vertex(&*self, labels, properties)?;
        self.store_vertex(id, labels, properties)
    }

    fn add_vertex_with(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<LayeredVertex, GraphError> {
        let (labels, properties) =
            write_rules::add_vertex_with(&*self, labels, properties, context)?;
        self.store_vertex(id, labels, properties)
    }

    /// The write rules of `insert_edge`; an edge between two added vertices is stored in the
    /// delta, one touching a base vertex is [`GraphError::Unsupported`] (task S1.4).
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: LayeredVertex,
        to: LayeredVertex,
        properties: PropertyMap,
    ) -> Result<LayeredEdge, GraphError> {
        let properties = write_rules::insert_edge(&*self, label.as_deref(), from, to, properties)?;
        self.store_edge(id, label, from, to, properties)
    }

    fn add_edge_with(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: LayeredVertex,
        to: LayeredVertex,
        properties: PropertyMap,
        context: &CreateContext,
    ) -> Result<LayeredEdge, GraphError> {
        let properties =
            write_rules::add_edge_with(&*self, label.as_deref(), from, to, properties, context)?;
        self.store_edge(id, label, from, to, properties)
    }

    fn add_vertex_label(
        &mut self,
        node: LayeredVertex,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        let Some(new_labels) = write_rules::add_vertex_label(&*self, node, &label)? else {
            return Ok(false);
        };
        delta_only(node.is_delta())?;
        self.delta
            .set_vertex_labels(node, new_labels, &mut self.statistics);
        Ok(true)
    }

    fn remove_vertex_label(
        &mut self,
        node: LayeredVertex,
        label: &str,
    ) -> Result<bool, GraphError> {
        let Some(new_labels) = write_rules::remove_vertex_label(&*self, node, label)? else {
            return Ok(false);
        };
        delta_only(node.is_delta())?;
        self.delta
            .set_vertex_labels(node, new_labels, &mut self.statistics);
        Ok(true)
    }

    fn set_edge_label(
        &mut self,
        edge: LayeredEdge,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        if !write_rules::set_edge_label(&*self, edge, &label)? {
            return Ok(false);
        }
        delta_only(edge.is_delta())?;
        self.delta.set_edge_label(edge, label, &mut self.statistics);
        Ok(true)
    }

    fn set_vertex_property(
        &mut self,
        node: LayeredVertex,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let value = write_rules::set_vertex_property(&*self, node, &key, value)?;
        delta_only(node.is_delta())?;
        self.delta.set_vertex_property(node, key, value);
        Ok(())
    }

    fn set_edge_property(
        &mut self,
        edge: LayeredEdge,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let value = write_rules::set_edge_property(&*self, edge, &key, value)?;
        delta_only(edge.is_delta())?;
        self.delta.set_edge_property(edge, key, value);
        Ok(())
    }

    /// `TraversalGraph`'s path write (the leaf coerced, the whole resulting property checked),
    /// not the trait default.
    fn set_vertex_property_path(
        &mut self,
        node: LayeredVertex,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let (key, value) = write_rules::set_vertex_property_path(&*self, node, path, value)?;
        delta_only(node.is_delta())?;
        self.delta.set_vertex_property(node, key, value);
        Ok(())
    }

    fn set_edge_property_path(
        &mut self,
        edge: LayeredEdge,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let (key, value) = write_rules::set_edge_property_path(&*self, edge, path, value)?;
        delta_only(edge.is_delta())?;
        self.delta.set_edge_property(edge, key, value);
        Ok(())
    }

    fn remove_vertex_property_path(
        &mut self,
        node: LayeredVertex,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        match write_rules::remove_vertex_property_path(&*self, node, path)? {
            PathRemoval::Nothing => {}
            PathRemoval::Drop(key) => {
                delta_only(node.is_delta())?;
                self.delta.drop_vertex_property(node, &key);
            }
            PathRemoval::Store(key, value) => {
                delta_only(node.is_delta())?;
                self.delta.set_vertex_property(node, key, value);
            }
        }
        Ok(())
    }

    fn remove_edge_property_path(
        &mut self,
        edge: LayeredEdge,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        match write_rules::remove_edge_property_path(&*self, edge, path)? {
            PathRemoval::Nothing => {}
            PathRemoval::Drop(key) => {
                delta_only(edge.is_delta())?;
                self.delta.drop_edge_property(edge, &key);
            }
            PathRemoval::Store(key, value) => {
                delta_only(edge.is_delta())?;
                self.delta.set_edge_property(edge, key, value);
            }
        }
        Ok(())
    }

    /// A stale handle is a no-op (idempotent drop), as in `TraversalGraph`.
    fn drop_vertex(&mut self, node: LayeredVertex) -> Result<(), GraphError> {
        if !write_rules::drop_vertex(&*self, node) {
            return Ok(());
        }
        delta_only(node.is_delta())?;
        self.delta.remove_vertex(node, &mut self.statistics);
        Ok(())
    }

    fn drop_vertex_property(&mut self, prop: &str, node: LayeredVertex) -> Result<(), GraphError> {
        if !write_rules::drop_vertex_property(&*self, node, prop)? {
            return Ok(());
        }
        delta_only(node.is_delta())?;
        self.delta.drop_vertex_property(node, prop);
        Ok(())
    }

    fn drop_edge(&mut self, edge: LayeredEdge) -> Result<(), GraphError> {
        if !write_rules::drop_edge(&*self, edge) {
            return Ok(());
        }
        delta_only(edge.is_delta())?;
        self.delta.remove_edge(edge, &mut self.statistics);
        Ok(())
    }

    fn drop_edge_property(&mut self, prop: &str, edge: LayeredEdge) -> Result<(), GraphError> {
        if !write_rules::drop_edge_property(&*self, edge, prop)? {
            return Ok(());
        }
        delta_only(edge.is_delta())?;
        self.delta.drop_edge_property(edge, prop);
        Ok(())
    }

    /// Raises the layer's own sequences (`max`, never lowered); the base's stay as they are.
    fn raise_id_sequences(
        &mut self,
        node_sequence: u64,
        edge_sequence: u64,
    ) -> Result<(), GraphError> {
        self.node_sequence = self.node_sequence.max(node_sequence);
        self.edge_sequence = self.edge_sequence.max(edge_sequence);
        Ok(())
    }
}

impl LayeredGraph {
    /// Stores a vertex the write rules accepted: its id checked, then the delta insert.
    fn store_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<LayeredVertex, GraphError> {
        let (id, sequence) = self.vertex_id_for(id)?;
        if let Err(err) = write_rules::check_vertex_id_free(&*self, &id) {
            self.node_sequence = sequence;
            return Err(err);
        }
        self.delta
            .insert_vertex(id, labels, properties, &mut self.statistics)
    }

    /// Stores an edge the write rules accepted (both endpoints live): an edge between two
    /// added vertices goes to the delta once its id is checked.
    fn store_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: LayeredVertex,
        to: LayeredVertex,
        properties: PropertyMap,
    ) -> Result<LayeredEdge, GraphError> {
        if !(from.is_delta() && to.is_delta()) {
            return Err(GraphError::unsupported(CROSS_EDGES));
        }
        let id = self.edge_id_for(id)?;
        self.delta
            .insert_edge(id, label, properties, from, to, &mut self.statistics)
    }
}
