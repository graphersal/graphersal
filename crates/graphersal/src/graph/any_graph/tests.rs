//! Every [`AnyGraph`] method on a `Graph<TraversalGraph>` gives what the direct
//! [`GraphTraversalSource`] call gives; a storage without compressed properties or snapshots
//! gets `Unsupported`.

use super::*;
use crate::graph::compression::CompressionDefinition;
use crate::graph::definition::QueryDefinition;
use crate::graph::handle::{EdgeHandle, VertexHandle};
use crate::graph::schema::ElementKind;
use crate::graph::storage::UnitMark;
use crate::graph::{RollbackReason, TraversalGraph, UnitChanges};
use crate::value::{ElementProperty, PropertyMap};
use compact_str::CompactString;

fn modern() -> (Arc<Graph>, Arc<dyn AnyGraph>) {
    let graph = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
    let any = graph.clone().into_any_graph();
    (graph, any)
}

const SCHEMA: &str = r#"{"mode": "open", "vertices": {"person": {"schema": {"type": "object",
    "properties": {"age": {"type": "integer"}}}}}}"#;

#[test]
fn identity_and_downcast() {
    let (graph, any) = modern();
    let same = any.as_any().downcast_ref::<Graph<TraversalGraph>>();
    assert!(same.is_some_and(|same| std::ptr::eq(same, &*graph)));
    assert!(any.as_any().downcast_ref::<Graph<Foreign>>().is_none());
    assert!(any.storage_type_name().ends_with("TraversalGraph"));
    let again = any.clone().into_any_graph();
    assert!(Arc::ptr_eq(&again, &any));
    assert_eq!(
        any.capabilities().unwrap(),
        graph.read().capabilities().unwrap()
    );
}

#[test]
fn statistics_memory_and_commit_position() {
    let (graph, any) = modern();
    assert_eq!(
        any.statistics().unwrap().as_ref(),
        graph.read().statistics().unwrap()
    );
    assert_eq!(
        any.memory_usage().unwrap(),
        graph.read().memory_usage().unwrap()
    );
    let before = any.last_commit_seq().unwrap();
    graph
        .write()
        .traversal_mut()
        .add_v("person")
        .to_list()
        .unwrap();
    assert_eq!(any.last_commit_seq().unwrap(), before + 1);
    assert_eq!(
        any.last_commit_seq().unwrap(),
        graph.read().last_commit_seq()
    );
    assert_eq!(any.statistics().unwrap().unwrap().vertex_count(), 7);
}

#[test]
fn schema_methods_match_the_traversal_source() {
    let (graph, any) = modern();
    let schema = GraphSchema::from_json(SCHEMA).unwrap();
    assert_eq!(any.get_schema().unwrap(), None);
    assert_eq!(
        any.infer_schema().unwrap(),
        graph.read().traversal().infer_schema().unwrap()
    );
    assert_eq!(
        any.validate_schema(&schema).unwrap(),
        graph.read().traversal().validate_schema(&schema).unwrap()
    );
    assert_eq!(
        any.diff_schema(&schema).unwrap(),
        graph.read().traversal().diff_schema(&schema).unwrap()
    );
    any.set_schema(schema.clone()).unwrap();
    assert_eq!(any.get_schema().unwrap(), Some(schema.clone()));
    assert_eq!(
        any.get_schema().unwrap(),
        graph.read().get_schema().unwrap()
    );
    assert!(any.diff_schema(&schema).unwrap().changes.is_empty());

    let patch =
        SchemaPatch::from_json(r#"{"vertices": {"software": {"schema": {"type": "object"}}}}"#)
            .unwrap();
    let validation = any.validate_schema_patch(&patch).unwrap();
    assert_eq!(
        validation,
        graph
            .read()
            .traversal()
            .validate_schema_patch(&patch)
            .unwrap()
    );
    let patched = any.patch_schema(&patch).unwrap();
    assert_eq!(any.get_schema().unwrap(), Some(patched));

    // A schema the data violates fails and changes nothing.
    let strict = GraphSchema::from_json(
        r#"{"mode": "closed", "vertices": {"person": {"schema": {"type": "object"}}}}"#,
    )
    .unwrap();
    let stored = any.get_schema().unwrap();
    assert!(any.set_schema(strict).is_err());
    assert_eq!(any.get_schema().unwrap(), stored);
}

fn query(name: &str) -> Definition {
    Definition::query(
        name,
        QueryDefinition::new(format!("fn {name}() {{ g.v() }}")),
    )
}

#[test]
fn definitions_are_stored_in_one_unit() {
    let (graph, any) = modern();
    let before = any.last_commit_seq().unwrap();
    any.set_definitions(vec![query("first"), query("second")])
        .unwrap();
    assert_eq!(
        any.last_commit_seq().unwrap(),
        before + 1,
        "one unit, one commit"
    );
    assert_eq!(
        any.definitions(Some(DefinitionKind::QUERY)).unwrap(),
        graph
            .read()
            .traversal()
            .list_definitions(Some(DefinitionKind::QUERY))
            .unwrap()
    );
    let names: Vec<_> = any
        .definitions(None)
        .unwrap()
        .iter()
        .map(|definition| definition.name().to_string())
        .collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(
        any.definition(DefinitionKind::QUERY, "first").unwrap(),
        graph
            .read()
            .traversal()
            .get_definition(DefinitionKind::QUERY, "first")
            .unwrap()
    );

    // A failing definition rolls the whole unit back.
    let position = any.last_commit_seq().unwrap();
    let failed = any.set_definitions(vec![query("third"), query("not a name")]);
    assert!(failed.is_err());
    assert!(
        any.definition(DefinitionKind::QUERY, "third")
            .unwrap()
            .is_none()
    );
    assert_eq!(any.definitions(None).unwrap().len(), 2);
    assert_eq!(any.last_commit_seq().unwrap(), position);

    any.move_query("first", Some("folder")).unwrap();
    any.describe_query("first", Some("All vertices.")).unwrap();
    let first = any
        .definition(DefinitionKind::QUERY, "first")
        .unwrap()
        .unwrap();
    let first = first.as_query().unwrap();
    assert_eq!(first.folder(), Some("folder"));
    assert_eq!(first.description(), Some("All vertices."));
    assert!(any.move_query("missing", None).is_err());

    assert!(
        any.remove_definition(DefinitionKind::QUERY, "first")
            .unwrap()
    );
    assert!(
        !any.remove_definition(DefinitionKind::QUERY, "first")
            .unwrap()
    );
    assert_eq!(any.definitions(None).unwrap().len(), 1);
}

fn compression_rule() -> Definition {
    let rule = CompressionDefinition::new(ElementKind::Vertex, "person", "bio").unwrap();
    Definition::compression("bio", rule)
}

#[test]
fn compression_on_traversal_graph() {
    let (_graph, any) = modern();
    any.set_definitions(vec![compression_rule()]).unwrap();
    any.recompress("bio").unwrap();
    assert!(any.compression_stats("bio").unwrap().is_some());
    assert!(any.recompress("missing").is_err());
    assert!(any.compression_stats("missing").unwrap().is_none());
}

#[test]
fn mark_without_journal_is_unavailable() {
    let (_graph, any) = modern();
    assert!(matches!(
        any.mark("before"),
        Err(GraphError::MarkUnavailable { .. })
    ));
}

#[cfg(feature = "io")]
#[test]
fn graphson_and_graphml_round_trips() {
    let (graph, any) = modern();
    let mut exported = Vec::new();
    any.export_graphson(&mut exported).unwrap();
    let mut direct = Vec::new();
    crate::io::GraphSONExport::new()
        .export_writer(&*graph.read(), &mut direct)
        .unwrap();
    assert_eq!(exported, direct);

    let target: Arc<dyn AnyGraph> = Arc::new(Graph::new(TraversalGraph::new()));
    let report = target
        .import_graphson(&mut exported.as_slice(), None)
        .unwrap();
    assert!(!report.has_skips());
    assert_eq!(target.statistics().unwrap(), any.statistics().unwrap());

    let mut xml = Vec::new();
    any.export_graphml(&mut xml, true).unwrap();
    let mut direct = Vec::new();
    crate::io::GraphMLExport::new()
        .with_beautify(true)
        .export_writer(&*graph.read(), &mut direct)
        .unwrap();
    assert_eq!(xml, direct);
    let target: Arc<dyn AnyGraph> = Arc::new(Graph::new(TraversalGraph::new()));
    target.import_graphml(&mut xml.as_slice(), None).unwrap();
    assert_eq!(target.statistics().unwrap(), any.statistics().unwrap());
    assert_eq!(
        target.last_commit_seq().unwrap(),
        1,
        "an import is one unit"
    );
}

#[cfg(feature = "persist")]
#[test]
fn write_snapshot_of_a_traversal_graph() {
    use crate::graph::PersistentStorage;
    let (graph, any) = modern();
    let mut bytes = Vec::new();
    let info = any.write_snapshot(&mut bytes).unwrap();
    assert_eq!((info.vertex_count, info.edge_count), (6, 6));
    let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    assert_eq!(loaded.graph_id(), graph.read().graph_id());
    assert_eq!(
        loaded.statistics().unwrap(),
        graph.read().statistics().unwrap()
    );
}

/// A storage that is not a `TraversalGraph`: it forwards the required methods, the catalog and
/// the units to one, and keeps every other default (no compressed properties, no statistics,
/// no snapshots).
#[derive(Default)]
struct Foreign(TraversalGraph);

impl GraphStorage for Foreign {
    type VertexId = VertexHandle;
    type EdgeId = EdgeHandle;

    fn vertex_count(&self) -> usize {
        self.0.vertex_count()
    }
    fn edge_count(&self) -> usize {
        self.0.edge_count()
    }
    fn vertices(&self) -> impl Iterator<Item = VertexHandle> + '_ {
        GraphStorage::vertices(&self.0)
    }
    fn edges(&self) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::edges(&self.0)
    }
    fn get_vertex_by_id(&self, id: &str) -> Option<VertexHandle> {
        GraphStorage::get_vertex_by_id(&self.0, id)
    }
    fn get_edge_by_id(&self, id: &str) -> Option<EdgeHandle> {
        GraphStorage::get_edge_by_id(&self.0, id)
    }
    fn out_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::out_edges(&self.0, node)
    }
    fn in_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
        GraphStorage::in_edges(&self.0, node)
    }
    fn vertex_id(&self, node: VertexHandle) -> Option<&str> {
        self.0.vertex_id(node)
    }
    fn edge_id(&self, edge: EdgeHandle) -> Option<&str> {
        self.0.edge_id(edge)
    }
    fn vertex_label(&self, node: VertexHandle) -> Option<&str> {
        self.0.vertex_label(node)
    }
    fn edge_label(&self, edge: EdgeHandle) -> Option<&str> {
        self.0.edge_label(edge)
    }
    fn vertex_labels(&self, node: VertexHandle) -> &[CompactString] {
        self.0.vertex_labels(node)
    }
    fn vertex_properties(&self, node: VertexHandle) -> Option<&PropertyMap> {
        self.0.vertex_properties(node)
    }
    fn edge_properties(&self, edge: EdgeHandle) -> Option<&PropertyMap> {
        self.0.edge_properties(edge)
    }
    fn edge_endpoints(&self, edge: EdgeHandle) -> Option<(VertexHandle, VertexHandle)> {
        self.0.edge_endpoints(edge)
    }
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<VertexHandle, GraphError> {
        self.0.insert_vertex(id, labels, properties)
    }
    fn add_vertex_label(
        &mut self,
        node: VertexHandle,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        self.0.add_vertex_label(node, label)
    }
    fn remove_vertex_label(&mut self, node: VertexHandle, label: &str) -> Result<bool, GraphError> {
        self.0.remove_vertex_label(node, label)
    }
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: VertexHandle,
        to: VertexHandle,
        properties: PropertyMap,
    ) -> Result<EdgeHandle, GraphError> {
        self.0.insert_edge(id, label, from, to, properties)
    }
    fn set_vertex_property(
        &mut self,
        node: VertexHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        self.0.set_vertex_property(node, key, value)
    }
    fn set_edge_property(
        &mut self,
        edge: EdgeHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        self.0.set_edge_property(edge, key, value)
    }
    fn drop_vertex(&mut self, node: VertexHandle) -> Result<(), GraphError> {
        self.0.drop_vertex(node)
    }
    fn drop_vertex_property(&mut self, prop: &str, node: VertexHandle) -> Result<(), GraphError> {
        self.0.drop_vertex_property(prop, node)
    }
    fn drop_edge(&mut self, edge: EdgeHandle) -> Result<(), GraphError> {
        self.0.drop_edge(edge)
    }
    fn drop_edge_property(&mut self, prop: &str, edge: EdgeHandle) -> Result<(), GraphError> {
        self.0.drop_edge_property(prop, edge)
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
    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        Ok(StorageCapabilities::none()
            .with_atomic(true)
            .with_writable(true))
    }
}

#[test]
fn a_foreign_storage_keeps_compression_rules_but_cannot_apply_them() {
    let any: Arc<dyn AnyGraph> = Arc::new(Graph::new(Foreign::default())).into_any_graph();
    assert!(any.as_any().downcast_ref::<Graph<Foreign>>().is_some());
    assert!(any.storage_type_name().ends_with("Foreign"));
    assert_eq!(any.statistics().unwrap(), None);

    // Decision 4: the rule is stored like any other definition ...
    any.set_definitions(vec![compression_rule(), query("people")])
        .unwrap();
    assert_eq!(
        any.definitions(Some(DefinitionKind::COMPRESSION))
            .unwrap()
            .len(),
        1
    );
    // ... but nothing applies it.
    let err = any.recompress("bio").unwrap_err();
    assert!(
        matches!(
            &*err,
            crate::error::TraverserError::Graph(GraphError::Unsupported { .. })
        ),
        "{err:?}"
    );
    assert!(any.compression_stats("bio").unwrap().is_none());
    assert!(matches!(
        any.mark("m"),
        Err(GraphError::MarkUnavailable { .. })
    ));
    assert!(
        any.remove_definition(DefinitionKind::COMPRESSION, "bio")
            .unwrap()
    );
}

#[cfg(feature = "persist")]
#[test]
fn a_foreign_storage_cannot_write_a_snapshot() {
    let any = Arc::new(Graph::new(Foreign::default())).into_any_graph();
    let mut bytes = Vec::new();
    assert!(matches!(
        any.write_snapshot(&mut bytes),
        Err(PersistError::Graph(GraphError::Unsupported { .. }))
    ));
    assert!(bytes.is_empty());
}

#[test]
fn a_foreign_storage_keeps_index_definitions_but_does_not_serve_them() {
    use crate::graph::property_index::PropertyIndexDefinition;
    let any = Arc::new(Graph::new(Foreign::default())).into_any_graph();
    let index = Definition::property_index(
        "person_age",
        PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
    );
    // The definition is stored like any other (a catalog file loaded into it) ...
    any.set_definitions(vec![index.clone()]).unwrap();
    assert_eq!(
        any.definitions(Some(DefinitionKind::PROPERTY_INDEX))
            .unwrap(),
        vec![index]
    );
    // ... but nothing serves it, and declaring one through the API is refused.
    assert!(any.property_indexes().unwrap().is_empty());
    assert!(!any.capabilities().unwrap().property_indexes);
    let mut foreign = Foreign::default();
    let err = crate::GraphTraversalSource::new(&mut foreign)
        .create_index(
            "x",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        )
        .unwrap_err();
    assert!(
        matches!(
            &*err,
            crate::error::TraverserError::Graph(GraphError::Unsupported { .. })
        ),
        "{err:?}"
    );
}
