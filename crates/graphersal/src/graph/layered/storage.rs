//! `impl GraphStorage for LayeredGraph`: the reads answer from the base (an untouched element
//! with one bit test, no lookup) merged with the delta; every write runs its write rule
//! (`graph/write_rules.rs`) first and then stores in the delta. Drops of base elements leave a
//! tombstone (S1.5); property, path and label writes to base elements go into images (S1.6,
//! `delta` module docs). The schema and the catalog are the layer's own copies (S1.7).

use compact_str::CompactString;

use super::LayeredGraph;
use super::delta::{PropertyChange, VertexView};
use super::handle::{LayeredEdge, LayeredVertex};
use crate::error::{DefinitionError, GraphError, SchemaError};
use crate::graph::definition::{Definition, DefinitionKind, MAX_DEFINITIONS};
use crate::graph::schema::defaults::CreateContext;
use crate::graph::schema::{ElementKind, GraphSchema, PropertySchema};
use crate::graph::topology::Direction;
use crate::graph::write_rules::{self, PathRemoval};
use crate::graph::{
    CompressionEstimate, CompressionEstimateRequest, CompressionStats, ExecutionPolicy,
    GraphNodeType, GraphStatistics, GraphStorage, PropertyIndexInfo, PropertyIndexRange,
    PropertyIndexTransform, StorageCapabilities, UniqueIndexDuplicates, VertexHandle,
};
use crate::value::{ElementProperty, JsonPath, PropertyMap};

/// The `operation` of the refusal of a schema with vector rules on a layer.
const VECTOR_RULES: &str =
    "vector rules (properties derived by an embedder) on a layered graph yet";

/// The `operation` of the refusal of a property index definition on a layer.
const PROPERTY_INDEXES: &str = "property index definitions on a layered graph yet";

/// The `operation` of the refusal of a compression rule on a layer.
const COMPRESSION_RULES: &str = "compression rules on a layered graph yet";

/// The `operation` of the refusal of an embedding model definition on a layer.
const MODELS: &str = "embedding model definitions on a layered graph yet";

/// Whether the layer's catalog can take (or remove) a definition of `kind`: saved queries,
/// jobs, host kinds and kinds this version does not know (kept opaque, as `TraversalGraph`
/// keeps them) need no storage machinery. Property indexes and compression rules need index
/// and value machinery over base + delta, models belong to the vector rules: refused until
/// the layer has index deltas (stage 4), with an error naming the limitation.
fn check_layer_kind(kind: DefinitionKind) -> Result<(), GraphError> {
    if kind.is_property_index() {
        Err(GraphError::unsupported(PROPERTY_INDEXES))
    } else if kind == DefinitionKind::COMPRESSION {
        Err(GraphError::unsupported(COMPRESSION_RULES))
    } else if kind == DefinitionKind::MODEL {
        Err(GraphError::unsupported(MODELS))
    } else {
        Ok(())
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

    /// The edges the layer added at `node` (an added vertex's own list, a dirty base vertex's
    /// stub), newest first, then the base's list (a delta vertex has none there: its slot is
    /// out of the base's range) without removed base edges (tested only for a dirty vertex).
    /// The same order as a `TraversalGraph` holding the same writes: it puts a new edge at the
    /// front of the lists. A self-loop is in both lists.
    #[inline]
    fn out_edges(
        &self,
        node: LayeredVertex,
    ) -> Result<impl Iterator<Item = LayeredEdge> + '_, GraphError> {
        let base = GraphStorage::out_edges(&*self.base, node.to_base())?
            .filter(self.base_adjacency_filter(node))
            .map(LayeredEdge::from_base);
        Ok(self
            .delta
            .edges_at(self.delta_adjacency_index(node), Direction::Outgoing)
            .chain(base))
    }

    /// See [`Self::out_edges`].
    #[inline]
    fn in_edges(
        &self,
        node: LayeredVertex,
    ) -> Result<impl Iterator<Item = LayeredEdge> + '_, GraphError> {
        let base = GraphStorage::in_edges(&*self.base, node.to_base())?
            .filter(self.base_adjacency_filter(node))
            .map(LayeredEdge::from_base);
        Ok(self
            .delta
            .edges_at(self.delta_adjacency_index(node), Direction::Incoming)
            .chain(base))
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

    /// The base's label index (no scan), without removed base vertices and those whose image
    /// lost `label` (one bit test per dirty member, the lost set fetched once per call), in
    /// the base bucket's order; then the delta's bucket: the added vertices and the base
    /// vertices that gained `label`, in the order of the writes.
    fn vertices_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Result<Box<dyn Iterator<Item = LayeredVertex> + 'a>, GraphError> {
        let clean = !self.dirty_vertices.any();
        let lost = self.delta.lost_vertex_label(label);
        Ok(Box::new(
            self.base
                .index
                .get_nodes_by_label(label)
                .into_iter()
                .flatten()
                .filter(move |node| {
                    let slot = node.index().slot();
                    clean
                        || !self.dirty_vertices.contains(slot)
                        || (!self.delta.is_removed_vertex(slot)
                            && lost.is_none_or(|lost| !lost.contains(slot)))
                })
                .map(LayeredVertex::from_base)
                .chain(self.delta.vertices_by_label(label)),
        ))
    }

    /// The base's label index without removed and relabeled base edges, then the base edges
    /// relabeled to `label` (in the order they gained it), then the added edges.
    fn edges_by_label<'a>(
        &'a self,
        label: &'a str,
    ) -> Result<Box<dyn Iterator<Item = LayeredEdge> + 'a>, GraphError> {
        let clean = !self.dirty_edges.any();
        let relabeled = self.delta.relabeled_edges();
        Ok(Box::new(
            self.base
                .index
                .get_edges_by_label(label)
                .into_iter()
                .flatten()
                .filter(move |edge| {
                    let slot = edge.index().slot();
                    clean
                        || !self.dirty_edges.contains(slot)
                        || (!self.delta.is_removed_edge(slot) && !relabeled.contains(slot))
                })
                .map(LayeredEdge::from_base)
                .chain(self.delta.gained_edges(label))
                .chain(self.delta.edges_by_label(label)),
        ))
    }

    /// The layer's own statistics: the base's at creation, kept exact by every write.
    #[inline]
    fn statistics(&self) -> Result<Option<&GraphStatistics>, GraphError> {
        Ok(Some(&self.statistics))
    }

    /// The layer's own schema (the base's copied at creation, then the layer's `set_schema`).
    fn get_schema(&self) -> Result<Option<GraphSchema>, GraphError> {
        Ok((!self.schema.is_empty()).then(|| self.schema.clone()))
    }

    fn declared_property_schema(
        &self,
        kind: GraphNodeType,
        labels: &[CompactString],
        key: &str,
    ) -> Result<Option<PropertySchema>, GraphError> {
        Ok(self.schema.declared_property(kind, labels, key).cloned())
    }

    /// Checks the layer's current state (base elements as the layer shows them, images, added
    /// elements) against `schema` exactly as `TraversalGraph` checks its own (only the labels
    /// whose declaration changed while the mode stays `Open`/`Closed`), the same errors, then
    /// stores it in the layer; the base's schema is never written. A schema with vector rules
    /// is refused (not supported on a layered graph yet).
    fn set_schema(&mut self, schema: GraphSchema) -> Result<(), GraphError> {
        if crate::graph::vector_rules::has_rules(&schema) {
            return Err(GraphError::unsupported(VECTOR_RULES));
        }
        // As `TraversalGraph::set_schema_recorded`: a vector property may name its model by
        // repository; the schema stores the identity of the catalog's current revision.
        let schema = crate::graph::model_definition::resolve_model_names(&self.catalog, schema);
        let report =
            crate::graph::schema::apply::scan_storage_change(&*self, Some(&self.schema), &schema)?;
        if !report.is_empty() {
            return Err(SchemaError::ExistingDataViolatesSchema {
                report: Box::new(report),
            }
            .into());
        }
        self.schema = schema;
        Ok(())
    }

    /// The layer's own catalog (the base's copied at creation, then the layer's writes).
    fn definitions(&self) -> Result<impl Iterator<Item = &Definition> + '_, GraphError> {
        Ok(self.catalog.iter())
    }

    fn definition(
        &self,
        kind: DefinitionKind,
        name: &str,
    ) -> Result<Option<&Definition>, GraphError> {
        Ok(self.catalog.get(kind, name))
    }

    fn set_definition(&mut self, definition: Definition) -> Result<(), GraphError> {
        self.set_definitions(vec![definition])
    }

    /// Stores each definition in turn as `TraversalGraph` does (validated, an equal one again
    /// is no change, at most `MAX_DEFINITIONS`), in the layer's catalog only, all or nothing:
    /// a refused definition leaves the catalog as it was, as on a `TraversalGraph`, which makes
    /// the list one unit of its own. A kind that needs storage machinery is refused
    /// (`check_layer_kind`).
    fn set_definitions(&mut self, definitions: Vec<Definition>) -> Result<(), GraphError> {
        // The catalog is small (at most `MAX_DEFINITIONS`) and written rarely: the changes go
        // into a copy that replaces the catalog once every definition passed.
        let mut catalog = self.catalog.clone();
        for definition in definitions {
            check_layer_kind(definition.kind())?;
            definition.validate()?;
            let current = catalog.get(definition.kind(), definition.name());
            if current == Some(&definition) {
                continue;
            }
            if current.is_none() && catalog.len() >= MAX_DEFINITIONS {
                return Err(DefinitionError::TooManyDefinitions {
                    limit: MAX_DEFINITIONS,
                }
                .into());
            }
            catalog.insert(definition);
        }
        self.catalog = catalog;
        Ok(())
    }

    /// Removes the definition from the layer's catalog (the base keeps its own); a kind that
    /// needs storage machinery is refused (`check_layer_kind`), also when the layer holds no
    /// definition of that name.
    fn remove_definition(&mut self, kind: DefinitionKind, name: &str) -> Result<bool, GraphError> {
        check_layer_kind(kind)?;
        Ok(self.catalog.remove(kind, name).is_some())
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

    /// The base's indexes. A lookup forwards to the base only while the layer has not added,
    /// removed or modified any element of the kind asked about; otherwise it answers `None`
    /// and the caller scans, so a lookup never misses an element the layer added or changed,
    /// nor returns one it removed or changed (index deltas: stage 4).
    fn property_indexes(&self) -> Result<Vec<PropertyIndexInfo>, GraphError> {
        GraphStorage::property_indexes(&*self.base)
    }

    fn vertices_by_property_index<'a>(
        &'a self,
        label: Option<&'a str>,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = LayeredVertex> + 'a>>, GraphError> {
        if self.delta.vertices_touched() {
            return Ok(None);
        }
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
        if self.delta.edges_touched() {
            return Ok(None);
        }
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
        if self.delta.vertices_touched() {
            return Ok(None);
        }
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
        if self.delta.edges_touched() {
            return Ok(None);
        }
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

    /// The base's count while the kind is untouched (see [`Self::property_indexes`]), else
    /// `None` (the caller counts).
    fn count_by_property_index(
        &self,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Result<Option<u64>, GraphError> {
        let touched = match element {
            ElementKind::Vertex => self.delta.vertices_touched(),
            ElementKind::Edge => self.delta.edges_touched(),
        };
        if touched {
            return Ok(None);
        }
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

    /// The write rules of `insert_edge`, then the id, then the edge is stored in the delta
    /// whatever its endpoints (a base endpoint through its stub).
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

    /// The rule first; a base vertex gets the new labels in its image (copied up on the first
    /// change).
    fn add_vertex_label(
        &mut self,
        node: LayeredVertex,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        let Some(new_labels) = write_rules::add_vertex_label(&*self, node, &label)? else {
            return Ok(false);
        };
        self.store_vertex_labels(node, new_labels)?;
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
        self.store_vertex_labels(node, new_labels)?;
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
        if edge.is_delta() {
            self.delta.set_edge_label(edge, label, &mut self.statistics);
        } else {
            self.dirty_edges.set(edge.slot());
            let base = edge.to_base();
            if self
                .delta
                .set_base_edge_label(&self.base, base, label, &mut self.statistics)?
            {
                self.delta.settle_base_edge(&self.base, base);
            }
        }
        Ok(true)
    }

    fn set_vertex_property(
        &mut self,
        node: LayeredVertex,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let value = write_rules::set_vertex_property(&*self, node, &key, value)?;
        self.change_vertex_property(node, PropertyChange::Set(key, value))
    }

    fn set_edge_property(
        &mut self,
        edge: LayeredEdge,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let value = write_rules::set_edge_property(&*self, edge, &key, value)?;
        self.change_edge_property(edge, PropertyChange::Set(key, value))
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
        self.change_vertex_property(node, PropertyChange::Set(key, value))
    }

    fn set_edge_property_path(
        &mut self,
        edge: LayeredEdge,
        path: &JsonPath,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        let (key, value) = write_rules::set_edge_property_path(&*self, edge, path, value)?;
        self.change_edge_property(edge, PropertyChange::Set(key, value))
    }

    fn remove_vertex_property_path(
        &mut self,
        node: LayeredVertex,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        match write_rules::remove_vertex_property_path(&*self, node, path)? {
            PathRemoval::Nothing => Ok(()),
            PathRemoval::Drop(key) => self.change_vertex_property(node, PropertyChange::Drop(key)),
            PathRemoval::Store(key, value) => {
                self.change_vertex_property(node, PropertyChange::Set(key, value))
            }
        }
    }

    fn remove_edge_property_path(
        &mut self,
        edge: LayeredEdge,
        path: &JsonPath,
    ) -> Result<(), GraphError> {
        match write_rules::remove_edge_property_path(&*self, edge, path)? {
            PathRemoval::Nothing => Ok(()),
            PathRemoval::Drop(key) => self.change_edge_property(edge, PropertyChange::Drop(key)),
            PathRemoval::Store(key, value) => {
                self.change_edge_property(edge, PropertyChange::Set(key, value))
            }
        }
    }

    /// A stale handle is a no-op (idempotent drop), as in `TraversalGraph`. A base vertex is
    /// tombstoned with its incident edges (`drop_base_vertex`).
    fn drop_vertex(&mut self, node: LayeredVertex) -> Result<(), GraphError> {
        if !write_rules::drop_vertex(&*self, node) {
            return Ok(());
        }
        if node.is_delta() {
            self.delta.remove_vertex(node, &mut self.statistics);
        } else {
            self.drop_base_vertex(node)?;
        }
        Ok(())
    }

    fn drop_vertex_property(&mut self, prop: &str, node: LayeredVertex) -> Result<(), GraphError> {
        if !write_rules::drop_vertex_property(&*self, node, prop)? {
            return Ok(());
        }
        self.change_vertex_property(node, PropertyChange::Drop(prop.into()))
    }

    /// A stale handle is a no-op; a base edge is tombstoned (`drop_base_edge`).
    fn drop_edge(&mut self, edge: LayeredEdge) -> Result<(), GraphError> {
        if !write_rules::drop_edge(&*self, edge) {
            return Ok(());
        }
        if edge.is_delta() {
            self.delta.remove_edge(edge, &mut self.statistics);
        } else {
            self.drop_base_edge(edge);
        }
        Ok(())
    }

    fn drop_edge_property(&mut self, prop: &str, edge: LayeredEdge) -> Result<(), GraphError> {
        if !write_rules::drop_edge_property(&*self, edge, prop)? {
            return Ok(());
        }
        self.change_edge_property(edge, PropertyChange::Drop(prop.into()))
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
    /// Stores the label list a label rule returned (a change: the rule answered `Some`): in the
    /// delta vertex, or in the image of the base vertex (copied up on its first change; a
    /// return to clean checked when it had one). The dirty bit is set before the copy-up.
    fn store_vertex_labels(
        &mut self,
        node: LayeredVertex,
        new_labels: Vec<CompactString>,
    ) -> Result<(), GraphError> {
        if node.is_delta() {
            self.delta
                .set_vertex_labels(node, new_labels, &mut self.statistics);
            return Ok(());
        }
        self.dirty_vertices.set(node.slot());
        let base = node.to_base();
        if self
            .delta
            .set_base_vertex_labels(&self.base, base, new_labels, &mut self.statistics)?
        {
            self.delta.settle_base_vertex(&self.base, base);
        }
        Ok(())
    }

    /// Stores one property change a write rule returned. A delta vertex takes it as it is; for
    /// a base vertex a change that changes nothing (plan P6: an identical value by
    /// `identity_eq`, a drop of an absent key) is skipped without an image, any other goes
    /// into its image (copied up on the first change; a return to clean checked when it had
    /// one).
    fn change_vertex_property(
        &mut self,
        node: LayeredVertex,
        change: PropertyChange,
    ) -> Result<(), GraphError> {
        if node.is_delta() {
            match change {
                PropertyChange::Set(key, value) => self.delta.set_vertex_property(node, key, value),
                PropertyChange::Drop(key) => self.delta.drop_vertex_property(node, &key),
            }
            return Ok(());
        }
        if change.is_noop(GraphStorage::vertex_properties(&*self, node)?) {
            return Ok(());
        }
        self.dirty_vertices.set(node.slot());
        let base = node.to_base();
        if self
            .delta
            .change_base_vertex_property(&self.base, base, change)?
        {
            self.delta.settle_base_vertex(&self.base, base);
        }
        Ok(())
    }

    /// The edge counterpart of [`Self::change_vertex_property`].
    fn change_edge_property(
        &mut self,
        edge: LayeredEdge,
        change: PropertyChange,
    ) -> Result<(), GraphError> {
        if edge.is_delta() {
            match change {
                PropertyChange::Set(key, value) => self.delta.set_edge_property(edge, key, value),
                PropertyChange::Drop(key) => self.delta.drop_edge_property(edge, &key),
            }
            return Ok(());
        }
        if change.is_noop(GraphStorage::edge_properties(&*self, edge)?) {
            return Ok(());
        }
        self.dirty_edges.set(edge.slot());
        let base = edge.to_base();
        if self
            .delta
            .change_base_edge_property(&self.base, base, change)?
        {
            self.delta.settle_base_edge(&self.base, base);
        }
        Ok(())
    }

    /// Drops the live base vertex `node` as `TraversalGraph::remove_vertex_raw` drops one: its
    /// incident edges first, each once (a self-loop from the outgoing list only), in the order
    /// of its lists (added edges at its stub, newest first, then its base edges), then the
    /// vertex itself (its tombstone; its stub goes with it). The vertex stays dirty.
    fn drop_base_vertex(&mut self, node: LayeredVertex) -> Result<(), GraphError> {
        let mut incident: Vec<LayeredEdge> = GraphStorage::out_edges(&*self, node)?.collect();
        for edge in GraphStorage::in_edges(&*self, node)? {
            if GraphStorage::edge_endpoints(&*self, edge)?.is_none_or(|(from, _)| from != node) {
                incident.push(edge);
            }
        }
        for edge in incident {
            if edge.is_delta() {
                self.delta.remove_edge(edge, &mut self.statistics);
            } else {
                self.drop_base_edge(edge);
            }
        }
        self.dirty_vertices.set(node.slot());
        self.delta
            .remove_base_vertex(&self.base, node.to_base(), &mut self.statistics);
        Ok(())
    }

    /// Drops the live base edge `edge`: its tombstone, and the dirty bits of the edge and of
    /// both endpoints (one for a self-loop), so the base adjacency lists of each endpoint are
    /// filtered (invariant of `base_adjacency_filter`).
    fn drop_base_edge(&mut self, edge: LayeredEdge) {
        let base = edge.to_base();
        if let Ok(Some((from, to))) = GraphStorage::edge_endpoints(&*self.base, base) {
            self.dirty_vertices.set(from.index().slot());
            self.dirty_vertices.set(to.index().slot());
        }
        self.dirty_edges.set(edge.slot());
        self.delta
            .remove_base_edge(&self.base, base, &mut self.statistics);
    }

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

    /// Stores an edge the write rules accepted (both endpoints live) once its id is checked:
    /// in the delta topology, a base endpoint through its stub (created on first use, after
    /// the id check, so a failed write leaves no stub and no dirty bit).
    fn store_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: LayeredVertex,
        to: LayeredVertex,
        properties: PropertyMap,
    ) -> Result<LayeredEdge, GraphError> {
        let id = self.edge_id_for(id)?;
        let from = self.edge_endpoint(from)?;
        let to = self.edge_endpoint(to)?;
        self.delta
            .insert_edge(id, label, properties, from, to, &mut self.statistics)
    }

    /// The delta node slot (with the endpoint handle's generation) a new edge at the live
    /// vertex `node` is linked to: the added vertex itself, or the stub of a base vertex,
    /// created when missing, with the vertex's dirty bit set (which also sets `any`).
    fn edge_endpoint(&mut self, node: LayeredVertex) -> Result<VertexHandle, GraphError> {
        if node.is_delta() {
            return Ok(node.to_delta());
        }
        let base = node.to_base();
        let stub = self.delta.stub_for(base)?;
        self.dirty_vertices.set(node.slot());
        Ok(VertexHandle::new(stub, base.generation()))
    }
}
