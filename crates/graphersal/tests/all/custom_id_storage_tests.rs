//! Proves the traversal pipeline is genuinely polymorphic over `GraphStorage::VertexId`/`EdgeId`,
//! not merely compiling for `TraversalGraph`'s `VertexHandle`/`EdgeHandle`. `CustomIdStorage` wraps
//! a `TraversalGraph` internally but exposes a distinct ID representation (a registry position).

use graphersal::error::GraphError;
use graphersal::prelude::{ElementProperty, PropertyMap};
use graphersal::schema::GraphSchema;
use graphersal::storage::{EdgeHandle, VertexHandle};
use graphersal::{prelude::TraversalGraph, storage::GraphStorage};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct CustomVertexId(u64);

impl std::fmt::Display for CustomVertexId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cv{}", self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct CustomEdgeId(u64);

impl std::fmt::Display for CustomEdgeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ce{}", self.0)
    }
}

/// A storage backend with a compound/distributed-style ID representation, distinct from
/// the inner handles, delegating storage to an inner `TraversalGraph`. Every element is added
/// through this wrapper, which registers its handle; the custom id is the registry position.
struct CustomIdStorage {
    inner: TraversalGraph,
    vertex_handles: Vec<VertexHandle>,
    vertex_ids: HashMap<VertexHandle, CustomVertexId>,
    edge_handles: Vec<EdgeHandle>,
    edge_ids: HashMap<EdgeHandle, CustomEdgeId>,
}

impl CustomIdStorage {
    fn new() -> Self {
        Self {
            inner: TraversalGraph::new(),
            vertex_handles: Vec::new(),
            vertex_ids: HashMap::new(),
            edge_handles: Vec::new(),
            edge_ids: HashMap::new(),
        }
    }

    fn vh(&self, id: CustomVertexId) -> VertexHandle {
        self.vertex_handles[id.0 as usize]
    }

    fn eh(&self, id: CustomEdgeId) -> EdgeHandle {
        self.edge_handles[id.0 as usize]
    }

    fn cv(&self, handle: VertexHandle) -> CustomVertexId {
        self.vertex_ids[&handle]
    }

    fn ce(&self, handle: EdgeHandle) -> CustomEdgeId {
        self.edge_ids[&handle]
    }
}

impl GraphStorage for CustomIdStorage {
    type VertexId = CustomVertexId;
    type EdgeId = CustomEdgeId;

    fn vertex_count(&self) -> usize {
        self.inner.vertex_count()
    }

    fn edge_count(&self) -> usize {
        self.inner.edge_count()
    }

    fn vertices(&self) -> impl Iterator<Item = CustomVertexId> + '_ {
        GraphStorage::vertices(&self.inner).map(|n| self.cv(n))
    }

    fn edges(&self) -> impl Iterator<Item = CustomEdgeId> + '_ {
        GraphStorage::edges(&self.inner).map(|e| self.ce(e))
    }

    fn get_vertex_by_id(&self, id: &str) -> Option<CustomVertexId> {
        GraphStorage::get_vertex_by_id(&self.inner, id).map(|n| self.cv(n))
    }

    fn get_edge_by_id(&self, id: &str) -> Option<CustomEdgeId> {
        GraphStorage::get_edge_by_id(&self.inner, id).map(|e| self.ce(e))
    }

    fn out_edges(&self, node: CustomVertexId) -> impl Iterator<Item = CustomEdgeId> + '_ {
        self.inner.out_edges(self.vh(node)).map(|e| self.ce(e))
    }

    fn in_edges(&self, node: CustomVertexId) -> impl Iterator<Item = CustomEdgeId> + '_ {
        self.inner.in_edges(self.vh(node)).map(|e| self.ce(e))
    }

    fn vertex_id(&self, node: CustomVertexId) -> Option<&str> {
        self.inner.vertex_id(self.vh(node))
    }

    fn edge_id(&self, edge: CustomEdgeId) -> Option<&str> {
        self.inner.edge_id(self.eh(edge))
    }

    fn vertex_label(&self, node: CustomVertexId) -> Option<&str> {
        self.inner.vertex_label(self.vh(node))
    }

    fn edge_label(&self, edge: CustomEdgeId) -> Option<&str> {
        self.inner.edge_label(self.eh(edge))
    }

    fn vertex_labels(&self, node: CustomVertexId) -> &[compact_str::CompactString] {
        self.inner.vertex_labels(self.vh(node))
    }

    fn vertex_properties(&self, node: CustomVertexId) -> Option<&PropertyMap> {
        self.inner.vertex_properties(self.vh(node))
    }

    fn edge_properties(&self, edge: CustomEdgeId) -> Option<&PropertyMap> {
        self.inner.edge_properties(self.eh(edge))
    }

    fn edge_endpoints(&self, edge: CustomEdgeId) -> Option<(CustomVertexId, CustomVertexId)> {
        self.inner
            .edge_endpoints(self.eh(edge))
            .map(|(from, to)| (self.cv(from), self.cv(to)))
    }

    fn insert_vertex(
        &mut self,
        id: Option<compact_str::CompactString>,
        labels: Vec<compact_str::CompactString>,
        properties: PropertyMap,
    ) -> Result<CustomVertexId, GraphError> {
        let handle = self.inner.insert_vertex(id, labels, properties)?;
        let id = CustomVertexId(self.vertex_handles.len() as u64);
        self.vertex_handles.push(handle);
        self.vertex_ids.insert(handle, id);
        Ok(id)
    }

    fn add_vertex_label(
        &mut self,
        node: CustomVertexId,
        label: compact_str::CompactString,
    ) -> Result<bool, GraphError> {
        self.inner.add_vertex_label(self.vh(node), label)
    }

    fn remove_vertex_label(
        &mut self,
        node: CustomVertexId,
        label: &str,
    ) -> Result<bool, GraphError> {
        self.inner.remove_vertex_label(self.vh(node), label)
    }

    fn insert_edge(
        &mut self,
        id: Option<compact_str::CompactString>,
        label: Option<compact_str::CompactString>,
        from: CustomVertexId,
        to: CustomVertexId,
        properties: PropertyMap,
    ) -> Result<CustomEdgeId, GraphError> {
        let (from, to) = (self.vh(from), self.vh(to));
        let handle = self.inner.insert_edge(id, label, from, to, properties)?;
        let id = CustomEdgeId(self.edge_handles.len() as u64);
        self.edge_handles.push(handle);
        self.edge_ids.insert(handle, id);
        Ok(id)
    }

    fn set_vertex_property(
        &mut self,
        node: CustomVertexId,
        key: compact_str::CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        self.inner.set_vertex_property(self.vh(node), key, value)
    }

    fn set_edge_property(
        &mut self,
        edge: CustomEdgeId,
        key: compact_str::CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        self.inner.set_edge_property(self.eh(edge), key, value)
    }

    fn drop_vertex(&mut self, node: CustomVertexId) -> Result<(), GraphError> {
        self.inner.drop_vertex(self.vh(node))
    }

    fn drop_vertex_property(&mut self, prop: &str, node: CustomVertexId) -> Result<(), GraphError> {
        self.inner.drop_vertex_property(prop, self.vh(node))
    }

    fn drop_edge(&mut self, edge: CustomEdgeId) -> Result<(), GraphError> {
        self.inner.drop_edge(self.eh(edge))
    }

    fn drop_edge_property(&mut self, prop: &str, edge: CustomEdgeId) -> Result<(), GraphError> {
        self.inner.drop_edge_property(prop, self.eh(edge))
    }

    fn get_schema(&self) -> Result<Option<GraphSchema>, GraphError> {
        self.inner.get_schema()
    }
}

fn build_custom_storage() -> CustomIdStorage {
    let mut storage = CustomIdStorage::new();

    let marko = storage
        .add_vertex(
            Some("1".into()),
            vec!["person".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let vadas = storage
        .add_vertex(
            Some("2".into()),
            vec!["person".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let lop = storage
        .add_vertex(
            Some("3".into()),
            vec!["software".into()],
            PropertyMap::default(),
        )
        .unwrap();

    storage
        .add_edge(
            Some("10".into()),
            Some("knows".into()),
            marko,
            vadas,
            PropertyMap::default(),
        )
        .unwrap();
    storage
        .add_edge(
            Some("11".into()),
            Some("created".into()),
            marko,
            lop,
            PropertyMap::default(),
        )
        .unwrap();

    storage
}

#[test]
fn test_custom_id_storage_traversal_pipeline() {
    let mut storage = build_custom_storage();
    let g = graphersal::GraphTraversalSource::new(&mut storage);

    // Full pipeline: source scan, flatmap expansion, filter, and barrier aggregation,
    // all driven purely through the CustomVertexId/CustomEdgeId associated types.
    let count = g
        .v("1")
        .out_e(None)
        .has_label("knows")
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(count, ElementProperty::Number(1i64.into()));
}

#[test]
fn test_custom_id_storage_optimizer_pushdown_pipeline() {
    let mut storage = build_custom_storage();
    let g = graphersal::GraphTraversalSource::new(&mut storage);

    // Exercises SourceFilterPushdownRule (has_label folded into v()) and CountPushdownRule.
    let count = g
        .v(None::<()>)
        .has_label("person")
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(count, ElementProperty::Number(2i64.into()));
}

#[test]
fn test_custom_id_storage_values_and_labels() {
    let mut storage = build_custom_storage();
    let g = graphersal::GraphTraversalSource::new(&mut storage);

    let names = g
        .v(None::<()>)
        .has_label("software")
        .label()
        .to_list()
        .unwrap();
    assert_eq!(names, vec![ElementProperty::String("software".into())]);
}

/// `CustomIdStorage` does not override the `*_property_path` methods, so jpath writes and
/// removals run through the trait's default implementations (read the top-level value, change a
/// copy, store the whole result).
#[test]
fn test_custom_id_storage_jpath_writes_use_the_default_trait_methods() {
    use graphersal::prelude::JsonPath;
    let path = |text: &str| text.parse::<JsonPath>().unwrap();
    let mut storage = build_custom_storage();

    // an index into an array that does not exist is a fault (arrays are never invented)
    let err = graphersal::GraphTraversalSource::new(&mut storage)
        .v("1")
        .property(path("meta.tags[0]"), "a")
        .to_list()
        .expect_err("no array to index");
    assert!(format!("{err:?}").contains("MissingArray"), "{err:?}");
    // a nested write creates the missing object, a second one extends it
    graphersal::GraphTraversalSource::new(&mut storage)
        .v("1")
        .property(path("meta.tags"), vec!["a"])
        .property(path("meta.level"), 3)
        .to_list()
        .unwrap();
    graphersal::GraphTraversalSource::new(&mut storage)
        .e("10")
        .property(path("info.since"), 2020)
        .to_list()
        .unwrap();
    let marko = storage.get_vertex_by_id("1").unwrap();
    let meta = storage
        .vertex_properties(marko)
        .unwrap()
        .get("meta")
        .cloned();
    let meta_of = |tags: Vec<ElementProperty>| {
        ElementProperty::from_entries(vec![
            ("tags".into(), ElementProperty::Array(tags)),
            ("level".into(), 3.into()),
        ])
    };
    assert_eq!(meta, Some(meta_of(vec!["a".into()])));
    let knows = storage.get_edge_by_id("10").unwrap();
    assert_eq!(
        storage.edge_properties(knows).unwrap().get("info").cloned(),
        Some(ElementProperty::from_entries(vec![(
            "since".into(),
            2020.into()
        )]))
    );

    // removing a nested key keeps the parent; a missing path changes nothing and is no error
    graphersal::GraphTraversalSource::new(&mut storage)
        .v("1")
        .remove_property(path("meta.tags[0]"))
        .remove_property(path("meta.nope.deeper"))
        .to_list()
        .unwrap();
    graphersal::GraphTraversalSource::new(&mut storage)
        .e("10")
        .remove_property(path("info.since"))
        .remove_property(path("missing.key"))
        .to_list()
        .unwrap();
    let meta = storage
        .vertex_properties(marko)
        .unwrap()
        .get("meta")
        .cloned();
    assert_eq!(meta, Some(meta_of(vec![])));
    assert_eq!(
        storage.edge_properties(knows).unwrap().get("info").cloned(),
        Some(ElementProperty::from_entries(Vec::<(
            ElementProperty,
            ElementProperty
        )>::new()))
    );

    // a write that descends into a scalar is a path fault, reported as such
    let err = graphersal::GraphTraversalSource::new(&mut storage)
        .v("1")
        .property(path("meta.level.x"), 1)
        .to_list()
        .expect_err("cannot descend into an int");
    let text = graphersal::error::format_diagnostic(&*err);
    assert!(text.contains("meta"), "{text}");
}
