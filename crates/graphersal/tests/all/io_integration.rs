#![cfg(feature = "io")]

use graphersal::GraphSource;
use graphersal::prelude::*;

#[cfg(feature = "io")]
#[test]
fn test_graphml_trait_roundtrip_file_and_stream() {
    let graph = GraphSource::empty();

    graph
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "Alice")
        .property("age", 30)
        .property("active", true)
        .next()
        .unwrap();

    graph
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "Bob")
        .property("age", 25)
        .property("active", false)
        .next()
        .unwrap();

    let temp_file = std::env::temp_dir().join("test_io_graphml_roundtrip.xml");
    let path_str = temp_file.to_str().unwrap();

    // Export via GraphExport trait
    graph
        .export_graphml(path_str)
        .expect("Export GraphML file failed");

    // Import via GraphSource::from_graphml
    let imported_graph = GraphSource::from_graphml(path_str).expect("Import GraphML file failed");

    let _ = std::fs::remove_file(&temp_file);

    let count = imported_graph
        .read()
        .traversal()
        .v(None::<()>)
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(count, ElementProperty::Number((2i64).into()));

    // Stream writer / reader test
    let mut buffer = Vec::new();
    graph
        .export_graphml_writer(&mut buffer)
        .expect("Export GraphML writer failed");

    let mut reader_graph = GraphSource::empty();
    reader_graph
        .import_graphml_reader(std::io::Cursor::new(buffer))
        .expect("Import GraphML reader failed");

    let reader_count = reader_graph
        .read()
        .traversal()
        .v(None::<()>)
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(reader_count, ElementProperty::Number((2i64).into()));
}

#[cfg(feature = "io")]
#[test]
fn test_traversal_graph_direct_trait_methods() {
    let graph = GraphSource::empty();
    graph
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "Charlie")
        .next()
        .unwrap();

    let temp_file = std::env::temp_dir().join("test_traversal_graph_io.xml");
    let path_str = temp_file.to_str().unwrap();

    graph.export_graphml(path_str).unwrap();

    let mut imported_graph = GraphSource::empty();
    imported_graph.import_graphml(path_str).unwrap();

    let _ = std::fs::remove_file(&temp_file);

    let count = imported_graph
        .read()
        .traversal()
        .v(None::<()>)
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(count, ElementProperty::Number((1i64).into()));
}

#[cfg(feature = "io")]
#[test]
fn test_compact_default_and_beautify_option() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();

    // Default compact GraphML export
    let mut compact_xml_buf = Vec::new();
    g.export_graphml_writer(&mut compact_xml_buf).unwrap();
    let compact_xml = String::from_utf8(compact_xml_buf).unwrap();

    // Compact XML should not contain indentation spaces before key element
    assert!(!compact_xml.contains("\n  <key "));

    // Beautified GraphML export via with("beautify", true)
    let mut beautified_xml_buf = Vec::new();
    lock.traversal()
        .with("beautify", true)
        .export_graphml_writer(&mut beautified_xml_buf)
        .unwrap();
    let beautified_xml = String::from_utf8(beautified_xml_buf).unwrap();

    assert!(beautified_xml.contains("\n  <key "));
}

#[cfg(feature = "io")]
#[test]
fn test_with_option_precedence_hierarchy() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = lock.traversal();

    // io.graphml.beautify takes precedence over io.beautify and beautify
    let g_override = g
        .with("beautify", false)
        .with("io.beautify", false)
        .with("io.graphml.beautify", true);

    let mut xml_buf = Vec::new();
    g_override.export_graphml_writer(&mut xml_buf).unwrap();
    let xml_str = String::from_utf8(xml_buf).unwrap();
    assert!(xml_str.contains("\n  <key "));

    // A format-specific option for another format leaves GraphML compact.
    let mut other_buf = Vec::new();
    lock.traversal()
        .with("beautify", false)
        .with("io.beautify", false)
        .export_graphml_writer(&mut other_buf)
        .unwrap();
    let other_str = String::from_utf8(other_buf).unwrap();
    assert!(!other_str.contains("\n  <key "));
}

#[cfg(feature = "io")]
#[test]
fn test_generic_custom_storage_io_roundtrip() {
    use graphersal::error::GraphError;
    use graphersal::io::{GraphMLExport, GraphMLImport};
    use graphersal::prelude::{ElementProperty, PropertyMap};
    use graphersal::storage::{EdgeHandle, GraphStorage, VertexHandle};

    // Minimal custom GraphStorage wrapper delegating internally to TraversalGraph
    struct CustomStorage {
        inner: graphersal::prelude::TraversalGraph,
    }

    impl CustomStorage {
        fn new() -> Self {
            Self {
                inner: graphersal::prelude::TraversalGraph::new(),
            }
        }
    }

    impl GraphStorage for CustomStorage {
        type VertexId = VertexHandle;
        type EdgeId = EdgeHandle;

        fn vertex_count(&self) -> Result<usize, GraphError> {
            GraphStorage::vertex_count(&self.inner)
        }
        fn edge_count(&self) -> Result<usize, GraphError> {
            GraphStorage::edge_count(&self.inner)
        }
        fn vertices(&self) -> Result<impl Iterator<Item = VertexHandle> + '_, GraphError> {
            GraphStorage::vertices(&self.inner)
        }
        fn edges(&self) -> Result<impl Iterator<Item = EdgeHandle> + '_, GraphError> {
            GraphStorage::edges(&self.inner)
        }
        fn get_vertex_by_id(&self, id: &str) -> Result<Option<VertexHandle>, GraphError> {
            self.inner.get_vertex_by_id(id)
        }
        fn get_edge_by_id(&self, id: &str) -> Result<Option<EdgeHandle>, GraphError> {
            self.inner.get_edge_by_id(id)
        }
        fn out_edges(
            &self,
            node: VertexHandle,
        ) -> Result<impl Iterator<Item = EdgeHandle> + '_, GraphError> {
            GraphStorage::out_edges(&self.inner, node)
        }
        fn in_edges(
            &self,
            node: VertexHandle,
        ) -> Result<impl Iterator<Item = EdgeHandle> + '_, GraphError> {
            GraphStorage::in_edges(&self.inner, node)
        }
        fn vertex_id(&self, node: VertexHandle) -> Result<Option<&str>, GraphError> {
            self.inner.vertex_id(node)
        }
        fn edge_id(&self, edge: EdgeHandle) -> Result<Option<&str>, GraphError> {
            self.inner.edge_id(edge)
        }
        fn vertex_label(&self, node: VertexHandle) -> Result<Option<&str>, GraphError> {
            self.inner.vertex_label(node)
        }
        fn edge_label(&self, edge: EdgeHandle) -> Result<Option<&str>, GraphError> {
            self.inner.edge_label(edge)
        }
        fn vertex_labels(
            &self,
            node: VertexHandle,
        ) -> Result<&[compact_str::CompactString], GraphError> {
            self.inner.vertex_labels(node)
        }
        fn vertex_properties(
            &self,
            node: VertexHandle,
        ) -> Result<Option<&PropertyMap>, GraphError> {
            self.inner.vertex_properties(node)
        }
        fn edge_properties(&self, edge: EdgeHandle) -> Result<Option<&PropertyMap>, GraphError> {
            self.inner.edge_properties(edge)
        }
        fn edge_endpoints(
            &self,
            edge: EdgeHandle,
        ) -> Result<Option<(VertexHandle, VertexHandle)>, GraphError> {
            self.inner.edge_endpoints(edge)
        }

        fn insert_vertex(
            &mut self,
            id: Option<compact_str::CompactString>,
            labels: Vec<compact_str::CompactString>,
            properties: PropertyMap,
        ) -> Result<VertexHandle, GraphError> {
            self.inner.insert_vertex(id, labels, properties)
        }

        fn add_vertex_label(
            &mut self,
            node: VertexHandle,
            label: compact_str::CompactString,
        ) -> Result<bool, GraphError> {
            self.inner.add_vertex_label(node, label)
        }

        fn remove_vertex_label(
            &mut self,
            node: VertexHandle,
            label: &str,
        ) -> Result<bool, GraphError> {
            self.inner.remove_vertex_label(node, label)
        }

        fn insert_edge(
            &mut self,
            id: Option<compact_str::CompactString>,
            label: Option<compact_str::CompactString>,
            from: VertexHandle,
            to: VertexHandle,
            properties: PropertyMap,
        ) -> Result<EdgeHandle, GraphError> {
            self.inner.insert_edge(id, label, from, to, properties)
        }

        fn set_vertex_property(
            &mut self,
            node: VertexHandle,
            key: compact_str::CompactString,
            value: ElementProperty,
        ) -> Result<(), GraphError> {
            self.inner.set_vertex_property(node, key, value)
        }

        fn set_edge_property(
            &mut self,
            edge: EdgeHandle,
            key: compact_str::CompactString,
            value: ElementProperty,
        ) -> Result<(), GraphError> {
            self.inner.set_edge_property(edge, key, value)
        }

        fn drop_vertex(&mut self, node: VertexHandle) -> Result<(), GraphError> {
            self.inner.drop_vertex(node)
        }

        fn drop_vertex_property(
            &mut self,
            prop: &str,
            node: VertexHandle,
        ) -> Result<(), GraphError> {
            self.inner.drop_vertex_property(prop, node)
        }

        fn drop_edge(&mut self, edge: EdgeHandle) -> Result<(), GraphError> {
            self.inner.drop_edge(edge)
        }

        fn drop_edge_property(&mut self, prop: &str, edge: EdgeHandle) -> Result<(), GraphError> {
            self.inner.drop_edge_property(prop, edge)
        }
    }

    // 1. Populate CustomStorage
    let mut custom_storage = CustomStorage::new();
    let mut v1_props = PropertyMap::default();
    v1_props.insert("name", ElementProperty::String("custom_v1".into()));
    let v1 = custom_storage
        .add_vertex(Some("10".into()), vec!["custom_node".into()], v1_props)
        .unwrap();

    let mut v2_props = PropertyMap::default();
    v2_props.insert("name", ElementProperty::String("custom_v2".into()));
    let v2 = custom_storage
        .add_vertex(Some("20".into()), vec!["custom_node".into()], v2_props)
        .unwrap();

    let mut e_props = PropertyMap::default();
    e_props.insert("weight", ElementProperty::Number(1.23.into()));
    custom_storage
        .add_edge(Some("100".into()), Some("connects".into()), v1, v2, e_props)
        .unwrap();

    // 2. Export GraphML from CustomStorage
    let mut xml_buf = Vec::new();
    let ml_exporter = GraphMLExport::new();
    ml_exporter
        .export_writer(&custom_storage, &mut xml_buf)
        .expect("Export XML failed");

    // 3. Import GraphML into new CustomStorage
    let mut imported_ml_storage = CustomStorage::new();
    let ml_importer = GraphMLImport {};
    let xml_reader = quick_xml::Reader::from_reader(std::io::Cursor::new(&xml_buf));
    ml_importer
        .import_reader(&mut imported_ml_storage, xml_reader)
        .expect("Import XML failed");

    assert_eq!(imported_ml_storage.vertex_count().unwrap(), 2);
    assert_eq!(imported_ml_storage.edge_count().unwrap(), 1);
    let ml_v1 = imported_ml_storage.get_vertex_by_id("10").unwrap().unwrap();
    assert_eq!(
        imported_ml_storage.vertex_label(ml_v1).unwrap(),
        Some("custom_node")
    );
}

/// Imports `xml` into an empty graph through the stream entry point.
#[cfg(feature = "io")]
fn import_graphml_str(xml: &str) -> Result<Graph, IOError> {
    use graphersal::io::GraphImport;
    let mut g = GraphSource::empty();
    g.import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()))?;
    Ok(g)
}

/// RUSTSEC-2026-0194: quick-xml < 0.41 checked a start tag for duplicate attribute names in
/// quadratic time, so a single tag with many attributes stalled the importer. The tag below has
/// 50 000 distinct attributes, then a duplicate and the `id` the importer looks for, so every
/// attribute is scanned: the import must fail cleanly with an XML parse error, and quickly.
#[cfg(feature = "io")]
#[test]
fn hostile_tag_with_many_attributes_fails_fast_with_a_parse_error() {
    let mut tag = String::with_capacity(1 << 20);
    tag.push_str("<node");
    for i in 0..50_000 {
        tag.push_str(&format!(" a{i}=\"\""));
    }
    tag.push_str(" a0=\"\" id=\"1\"/>");
    let xml = format!(
        r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns"><graph id="G">{tag}</graph></graphml>"#
    );

    let started = std::time::Instant::now();
    let res = import_graphml_str(&xml);
    let elapsed = started.elapsed();

    assert!(
        matches!(res, Err(IOError::XmlParse { .. })),
        "expected XmlParse, got {:?}",
        res.err()
    );
    // A generous bound for slow CI machines; the quadratic check is orders of magnitude slower.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "import took {elapsed:?}"
    );
}

/// Entity and character references in `<data>` text resolve into one value (quick-xml reports
/// them as separate events), an unknown entity is a parse error, and attribute values keep
/// their whitespace as written.
#[cfg(feature = "io")]
#[test]
fn data_text_references_and_attribute_whitespace_import_unchanged() {
    let head = r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
      <key id="name" for="node" attr.name="name" attr.type="string"/>
      <graph id="G">"#;
    let g = import_graphml_str(&format!(
        "{head}<node id=\"a\tb\"><data key=\"name\">x &amp; y &#65;&lt;</data></node></graph></graphml>"
    ))
    .expect("import");
    let names = g
        .read()
        .traversal()
        .v("a\tb")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names, vec![ElementProperty::String("x & y A<".into())]);

    let res = import_graphml_str(&format!(
        "{head}<node id=\"1\"><data key=\"name\">x &bogus; y</data></node></graph></graphml>"
    ));
    assert!(
        matches!(res, Err(IOError::XmlParse { .. })),
        "expected XmlParse, got {:?}",
        res.err()
    );
}
