//! `impl GraphStorage for LayeredGraph`: the reads answer from the base (an untouched element
//! with one bit test, no lookup) merged with the delta; the writes are not implemented yet.

use compact_str::CompactString;

use super::handle::{LayeredEdge, LayeredVertex};
use super::{LayeredGraph, delta::VertexView};
use crate::error::GraphError;
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::schema::{ElementKind, GraphSchema, PropertySchema};
use crate::graph::{
    CompressionEstimate, CompressionEstimateRequest, CompressionStats, ExecutionPolicy,
    GraphNodeType, GraphStatistics, GraphStorage, PropertyIndexInfo, PropertyIndexRange,
    PropertyIndexTransform, StorageCapabilities, UniqueIndexDuplicates,
};
use crate::value::{ElementProperty, PropertyMap};

/// The operation every write names until the layer's write path exists.
const WRITES: &str = "writes to a layered graph yet";

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
            |view| Ok(view.and_then(|e| e.label)),
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

    /// The base's statistics (the layer changes nothing yet).
    #[inline]
    fn statistics(&self) -> Result<Option<&GraphStatistics>, GraphError> {
        GraphStorage::statistics(&*self.base)
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

    /// Neither `writable` nor `atomic` yet (writes are not implemented); the property index
    /// lookups are the base's, so they are exact when the base's are.
    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        let base = GraphStorage::capabilities(&*self.base)?;
        Ok(StorageCapabilities::none()
            .with_exact_property_index_lookups(base.exact_property_index_lookups))
    }

    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<LayeredVertex, GraphError> {
        let _ = (id, labels, properties);
        Err(GraphError::unsupported(WRITES))
    }

    fn add_vertex_label(
        &mut self,
        node: LayeredVertex,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        let _ = (node, label);
        Err(GraphError::unsupported(WRITES))
    }

    fn remove_vertex_label(
        &mut self,
        node: LayeredVertex,
        label: &str,
    ) -> Result<bool, GraphError> {
        let _ = (node, label);
        Err(GraphError::unsupported(WRITES))
    }

    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: LayeredVertex,
        to: LayeredVertex,
        properties: PropertyMap,
    ) -> Result<LayeredEdge, GraphError> {
        let _ = (id, label, from, to, properties);
        Err(GraphError::unsupported(WRITES))
    }

    fn set_vertex_property(
        &mut self,
        node: LayeredVertex,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let _ = (node, key, value);
        Err(GraphError::unsupported(WRITES))
    }

    fn set_edge_property(
        &mut self,
        edge: LayeredEdge,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let _ = (edge, key, value);
        Err(GraphError::unsupported(WRITES))
    }

    fn drop_vertex(&mut self, node: LayeredVertex) -> Result<(), GraphError> {
        let _ = node;
        Err(GraphError::unsupported(WRITES))
    }

    fn drop_vertex_property(&mut self, prop: &str, node: LayeredVertex) -> Result<(), GraphError> {
        let _ = (prop, node);
        Err(GraphError::unsupported(WRITES))
    }

    fn drop_edge(&mut self, edge: LayeredEdge) -> Result<(), GraphError> {
        let _ = edge;
        Err(GraphError::unsupported(WRITES))
    }

    fn drop_edge_property(&mut self, prop: &str, edge: LayeredEdge) -> Result<(), GraphError> {
        let _ = (prop, edge);
        Err(GraphError::unsupported(WRITES))
    }
}
