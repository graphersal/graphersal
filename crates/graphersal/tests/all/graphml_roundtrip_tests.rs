#![cfg(feature = "io")]

use graphersal::io::{GraphExport, GraphImport, GraphMLExport};
use graphersal::prelude::*;
use graphersal::schema::ValueType;

#[test]
fn test_graphml_export_import_roundtrip_integration() {
    let graph = GraphSource::empty();

    graph
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "Marko")
        .property("age", 29)
        .property("is_active", true)
        .property("rating", 4.85)
        .next()
        .expect("should add v1");

    graph
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", "Vadas")
        .property("name", "Vadas")
        .property("age", 27)
        .property("is_active", false)
        .property("rating", 3.50)
        .next()
        .expect("should add v2");

    let temp_path = std::env::temp_dir()
        .join("graphml_roundtrip_test.graphml")
        .to_str()
        .unwrap()
        .to_string();

    graph
        .export_graphml(&temp_path)
        .expect("Export should succeed");

    let mut imported_graph = GraphSource::empty();
    imported_graph
        .import_graphml(&temp_path)
        .expect("Import should succeed");

    let _ = std::fs::remove_file(temp_path);

    let lock = imported_graph.read();

    let count = lock
        .traversal()
        .v(None::<()>)
        .count()
        .next()
        .unwrap()
        .unwrap();
    if let ElementProperty::Number(num) = count {
        assert_eq!(num.as_i64(), Some(2));
    } else {
        panic!("Expected 2 vertices");
    }

    let marko_names = lock
        .traversal()
        .v(None::<()>)
        .has_p("name", P::Eq("Marko"))
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(marko_names.len(), 1);
    assert_eq!(marko_names[0], ElementProperty::String("Marko".into()));

    let marko_active = lock
        .traversal()
        .v(None::<()>)
        .has_p("name", P::Eq("Marko"))
        .values("is_active")
        .to_list()
        .unwrap();
    assert_eq!(marko_active.len(), 1);
    assert_eq!(marko_active[0], ElementProperty::Boolean(true));

    let vadas_active = lock
        .traversal()
        .v(None::<()>)
        .has_p("name", P::Eq("Vadas"))
        .values("is_active")
        .to_list()
        .unwrap();
    assert_eq!(vadas_active.len(), 1);
    assert_eq!(vadas_active[0], ElementProperty::Boolean(false));
}

/// A UUID value round-trips through GraphML as `attr.type="string"` (design decision D7): the
/// physical encoding is the canonical string, and re-importing it with no schema present yields
/// a plain string, never `ElementProperty::Uuid` (design decision D3 — no content-sniffing).
#[test]
fn test_graphml_roundtrip_of_a_uuid_value_is_string_typed() {
    let canonical = "550e8400-e29b-41d4-a716-446655440000";
    let uuid_value = ElementProperty::from(canonical)
        .cast(&ValueType::Uuid)
        .expect("a canonical UUID string casts to ValueType::Uuid");

    let graph = GraphSource::empty();
    graph
        .write()
        .traversal_mut()
        .add_v("entity")
        .property("external_id", uuid_value)
        .next()
        .expect("should add the vertex");

    let mut buf = Vec::new();
    GraphMLExport::new()
        .export_writer(&*graph.read(), &mut buf)
        .unwrap();
    let xml = String::from_utf8(buf).unwrap();

    assert!(
        xml.contains(
            "<key id=\"external_id\" for=\"node\" attr.name=\"external_id\" attr.type=\"string\"/>"
        ),
        "a UUID property has no dedicated GraphML type; it must declare as string, got: {xml}"
    );
    assert!(
        xml.contains(&format!("<data key=\"external_id\">{canonical}</data>")),
        "the canonical rendering must survive export byte-identical, got: {xml}"
    );

    let temp_path = std::env::temp_dir()
        .join("graphml_uuid_roundtrip_test.graphml")
        .to_str()
        .unwrap()
        .to_string();
    graph
        .export_graphml(&temp_path)
        .expect("export should succeed");

    let mut imported_graph = GraphSource::empty();
    imported_graph
        .import_graphml(&temp_path)
        .expect("import should succeed");
    let _ = std::fs::remove_file(temp_path);

    let lock = imported_graph.read();
    let reimported = lock
        .traversal()
        .v(None::<()>)
        .values("external_id")
        .to_list()
        .unwrap();
    assert_eq!(reimported.len(), 1);
    // Not `ElementProperty::Uuid`: no schema was declared, so no semantic type is inferred.
    assert_eq!(reimported[0], ElementProperty::from(canonical));
}

#[test]
fn test_schema_compatibility_structure() {
    let graph = GraphSource::empty();

    graph
        .write()
        .traversal_mut()
        .add_v("software")
        .property("name", "Graphersal")
        .property("lang", "Rust")
        .property("version", 1)
        .next()
        .unwrap();

    let mut buf = Vec::new();
    let exporter = GraphMLExport::new();
    exporter.export_writer(&*graph.read(), &mut buf).unwrap();

    let xml = String::from_utf8(buf).unwrap();

    assert!(xml.contains("xmlns=\"http://graphml.graphdrawing.org/xmlns\""));
    assert!(
        xml.contains("<key id=\"labelV\" for=\"node\" attr.name=\"labelV\" attr.type=\"string\"/>")
    );
    assert!(
        xml.contains("<key id=\"lang\" for=\"node\" attr.name=\"lang\" attr.type=\"string\"/>")
    );
    assert!(
        xml.contains("<key id=\"version\" for=\"node\" attr.name=\"version\" attr.type=\"long\"/>")
    );
    assert!(xml.contains("<graph id=\"G\" edgedefault=\"directed\">"));
    assert!(xml.contains("<data key=\"labelV\">software</data>"));
    assert!(xml.contains("<data key=\"lang\">Rust</data>"));
    assert!(xml.contains("<data key=\"version\">1</data>"));
}

// ---------------------------------------------------------------------------------------------
// Type system step 6: GraphML through the physical codec.
// ---------------------------------------------------------------------------------------------

mod codec {
    use graphersal::io::{GraphImport, GraphMLExport};
    use graphersal::prelude::*;
    use graphersal::schema::{PropertySchema, ValueType};

    const CANONICAL: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn uuid() -> ElementProperty {
        ElementProperty::uuid_from_string(CANONICAL).expect("canonical UUID")
    }

    fn export<S: GraphStorage>(storage: &S) -> String {
        let mut buf = Vec::new();
        GraphMLExport::new()
            .export_writer(storage, &mut buf)
            .expect("export succeeds");
        String::from_utf8(buf).expect("GraphML is UTF-8")
    }

    /// Imports `xml` into an empty graph whose stored schema is `schema`, if any.
    fn import(xml: &str, schema: Option<GraphSchema>) -> Result<Graph, IOError> {
        let mut graph = GraphSource::empty();
        if let Some(schema) = schema {
            graph.write().set_schema(schema).unwrap();
        }
        graph.import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()))?;
        Ok(graph)
    }

    /// The schema node of a value type: a union is `anyOf`, an object declares no keys.
    pub(super) fn node(ty: &ValueType) -> PropertySchema {
        match ty {
            ValueType::Null => PropertySchema::null(),
            ValueType::Boolean => PropertySchema::boolean(),
            ValueType::Int64 => PropertySchema::integer(),
            ValueType::Float64 => PropertySchema::number(),
            ValueType::String => PropertySchema::string(),
            ValueType::Uuid => PropertySchema::uuid(),
            ValueType::Array(item) => PropertySchema::array(node(item)),
            ValueType::Object => PropertySchema::object(),
            ValueType::Union(members) => PropertySchema::any_of(members.iter().map(node).collect()),
            other => panic!("no node for {other}"),
        }
    }

    /// A schema whose vertex labels declare the given `(label, [(key, type)])` fields, each
    /// optional and nullable.
    fn schema_with(mode: SchemaMode, labels: &[(&str, &[(&str, ValueType)])]) -> GraphSchema {
        let mut schema = GraphSchema::new().with_mode(mode);
        for (label, fields) in labels {
            let mut root = PropertySchema::object();
            for (key, ty) in fields.iter() {
                root = root.with_property(*key, node(ty).nullable());
            }
            schema = schema.with_vertex(*label, root);
        }
        schema
    }

    fn add_vertex(graph: &Graph, label: &str, props: Vec<(&str, ElementProperty)>) {
        let mut element = PropertyMap::default();
        for (key, value) in props {
            element.insert(key, value);
        }
        graph
            .write()
            .add_vertex(None, vec![label.into()], element)
            .expect("vertex is added");
    }

    fn single_value(graph: &Graph, key: &str) -> ElementProperty {
        let values = graph
            .read()
            .traversal()
            .v(None::<()>)
            .values(key)
            .to_list()
            .expect("values() succeeds");
        assert_eq!(values.len(), 1, "exactly one `{key}` value, got {values:?}");
        values.into_iter().next().unwrap_or(ElementProperty::Null)
    }

    /// A graph with one `entity` vertex holding `uid: Uuid`.
    fn uuid_graph() -> Graph {
        let graph = GraphSource::empty();
        add_vertex(&graph, "entity", vec![("uid", uuid())]);
        graph
    }

    fn entity_uid_schema(mode: SchemaMode) -> GraphSchema {
        schema_with(mode, &[("entity", &[("uid", ValueType::Uuid)])])
    }

    fn assert_no_debug_text(xml: &str) {
        for debug in ["Object(", "Array(", "EdgeIndex(", "NodeIndex(", "Uuid("] {
            assert!(
                !xml.contains(debug),
                "`{debug}` Debug text in GraphML: {xml}"
            );
        }
    }

    #[test]
    fn uuid_exports_canonically_as_a_string_key_and_int64_as_long() {
        let graph = uuid_graph();
        add_vertex(&graph, "entity", vec![("age", 29i64.into())]);
        let xml = export(&*graph.read());

        assert!(
            xml.contains(r#"<key id="uid" for="node" attr.name="uid" attr.type="string"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(&format!(r#"<data key="uid">{CANONICAL}</data>"#)),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<key id="age" for="node" attr.name="age" attr.type="long"/>"#),
            "int64 is 64-bit: GraphML `long`, not the 32-bit `int`: {xml}"
        );
        assert!(xml.contains(r#"<data key="age">29</data>"#), "{xml}");
        assert_no_debug_text(&xml);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn containers_export_as_json_text_under_a_string_key() {
        let mut inner = PropertyMap::default();
        inner.insert("id", uuid());
        inner.insert("n", ElementProperty::Number(Number::from_f64(1.0)));
        let object = ElementProperty::Object(Box::new(inner));
        let array = ElementProperty::Array(vec![
            1i64.into(),
            2.5f64.into(),
            "a \"quoted\" <tag>".into(),
            true.into(),
        ]);
        let graph = GraphSource::empty();
        add_vertex(&graph, "doc", vec![("obj", object), ("arr", array)]);

        let xml = export(&*graph.read());
        assert!(
            xml.contains(r#"<key id="obj" for="node" attr.name="obj" attr.type="string"/>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<key id="arr" for="node" attr.name="arr" attr.type="string"/>"#),
            "{xml}"
        );
        assert_no_debug_text(&xml);

        // Undeclared, both come back as the JSON text they were written as.
        let imported = import(&xml, None).expect("import succeeds");
        let obj_text = single_value(&imported, "obj");
        let arr_text = single_value(&imported, "arr");
        let obj: serde_json::Value =
            serde_json::from_str(obj_text.as_str().expect("a string")).expect("valid JSON");
        let arr: serde_json::Value =
            serde_json::from_str(arr_text.as_str().expect("a string")).expect("valid JSON");
        assert_eq!(obj, serde_json::json!({"id": CANONICAL, "n": 1.0}));
        assert_eq!(arr, serde_json::json!([1, 2.5, "a \"quoted\" <tag>", true]));
    }

    #[test]
    fn uuid_round_trip_without_a_schema_is_a_string() {
        let xml = export(&*uuid_graph().read());
        let imported = import(&xml, None).expect("import succeeds");
        assert_eq!(
            single_value(&imported, "uid"),
            ElementProperty::from(CANONICAL)
        );
    }

    #[test]
    fn uuid_round_trip_with_a_stored_declaration_restores_uuid_in_every_mode() {
        let xml = export(&*uuid_graph().read());
        for mode in [SchemaMode::None, SchemaMode::Open, SchemaMode::Closed] {
            let imported = import(&xml, Some(entity_uid_schema(mode))).expect("import succeeds");
            assert_eq!(single_value(&imported, "uid"), uuid(), "mode {mode}");
        }
    }

    #[test]
    fn multi_label_vertex_uses_the_first_declaring_label() {
        let xml = format!(
            r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
              <key id="uid" for="node" attr.name="uid" attr.type="string"/>
              <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
              <graph id="G" edgedefault="directed">
                <node id="1"><data key="uid">{CANONICAL}</data><data key="labelV">plain::entity</data></node>
              </graph>
            </graphml>"#
        );
        // `plain` declares nothing for `uid`; the second label `entity` does.
        let schema = schema_with(
            SchemaMode::None,
            &[
                ("plain", &[("name", ValueType::String)]),
                ("entity", &[("uid", ValueType::Uuid)]),
            ],
        );
        let imported = import(&xml, Some(schema)).expect("import succeeds");
        assert_eq!(single_value(&imported, "uid"), uuid());
    }

    #[test]
    fn non_canonical_uuid_into_a_declared_uuid_fails() {
        let xml = r#"<graphml>
          <key id="uid" for="node" attr.name="uid" attr.type="string"/>
          <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
          <graph><node id="1"><data key="labelV">entity</data><data key="uid">550E8400-E29B-41D4-A716-446655440000</data></node></graph>
        </graphml>"#;
        let err = import(xml, Some(entity_uid_schema(SchemaMode::None)))
            .expect_err("a non-canonical UUID is rejected");
        match err {
            IOError::InvalidAttributeValue {
                key,
                value,
                expected_type,
                ..
            } => {
                assert_eq!(key, "uid");
                assert_eq!(value, "550E8400-E29B-41D4-A716-446655440000");
                assert_eq!(expected_type, "uuid");
            }
            other => panic!("expected InvalidAttributeValue, got {other:?}"),
        }
    }

    /// An import into a live graph is one unit (`book/src/users_guide/transactions.md`): when an element
    /// is rejected, the vertices imported before it are rolled back too.
    #[test]
    fn import_into_a_closed_schema_is_atomic() {
        let xml = r#"<graphml>
          <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
          <graph>
            <node id="1"><data key="labelV">entity</data></node>
            <node id="2"><data key="labelV">undeclared</data></node>
          </graph>
        </graphml>"#;
        let mut graph = GraphSource::empty();
        graph
            .write()
            .set_schema(schema_with(SchemaMode::Closed, &[("entity", &[])]))
            .unwrap();
        let result = graph.import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()));
        assert!(
            result.is_err(),
            "the undeclared label is rejected in Closed mode"
        );
        assert_eq!(
            graph
                .read()
                .traversal()
                .v(None::<()>)
                .to_list()
                .unwrap()
                .len(),
            0,
            "the first vertex is rolled back with the failing import"
        );
        assert_eq!(
            graph.read().last_commit_seq(),
            1,
            "only the schema change was committed"
        );
    }

    /// F2: a property declared as an `array`/`object` (also as a union member without a text
    /// alternative) is read from its JSON text into the declared container.
    #[test]
    fn declared_containers_are_read_from_their_json_text() {
        let xml = r#"<graphml>
          <key id="tags" for="node" attr.name="tags" attr.type="string"/>
          <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
          <graph><node id="1"><data key="labelV">doc</data><data key="tags">["a", "b"]</data></node></graph>
        </graphml>"#;
        let expected = ElementProperty::Array(vec!["a".into(), "b".into()]);
        for declared in [
            ValueType::Array(Box::new(ValueType::String)),
            ValueType::Union(vec![
                ValueType::Int64,
                ValueType::Array(Box::new(ValueType::String)),
            ]),
        ] {
            let schema = schema_with(SchemaMode::None, &[("doc", &[("tags", declared.clone())])]);
            let graph = import(xml, Some(schema)).expect("the JSON text is imported");
            assert_eq!(
                single_value(&graph, "tags"),
                expected,
                "declared {declared}"
            );
        }

        // An object keeps its member order; nested values get their exact types.
        let xml = r#"<graphml>
          <key id="config" for="node" attr.name="config" attr.type="string"/>
          <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
          <graph><node id="1"><data key="labelV">doc</data><data key="config">{"z":1,"a":{"cpu":2.5,"on":true}}</data></node></graph>
        </graphml>"#;
        let schema = schema_with(
            SchemaMode::Open,
            &[("doc", &[("config", ValueType::Object)])],
        );
        let graph = import(xml, Some(schema)).expect("the object is imported");
        let mut inner = PropertyMap::default();
        inner.insert("cpu", 2.5f64.into());
        inner.insert("on", true.into());
        let mut outer = PropertyMap::default();
        outer.insert("z", 1i64.into());
        outer.insert("a", ElementProperty::Object(Box::new(inner)));
        let value = single_value(&graph, "config");
        assert_eq!(value, ElementProperty::Object(Box::new(outer)));
        let ElementProperty::Object(map) = value else {
            unreachable!()
        };
        assert_eq!(
            map.keys().map(|k| k.as_str()).collect::<Vec<_>>(),
            ["z", "a"]
        );
    }

    /// Without a declaration the JSON text stays a string (CLAUDE.md rule 7: never sniff).
    #[test]
    fn undeclared_json_text_stays_a_string() {
        let xml = r#"<graphml>
          <key id="tags" for="node" attr.name="tags" attr.type="string"/>
          <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
          <graph><node id="1"><data key="labelV">doc</data><data key="tags">["a"]</data></node></graph>
        </graphml>"#;
        let graph = import(xml, None).expect("import succeeds");
        assert_eq!(
            single_value(&graph, "tags"),
            ElementProperty::from(r#"["a"]"#)
        );
        let declared_elsewhere = schema_with(
            SchemaMode::None,
            &[(
                "other",
                &[("tags", ValueType::Array(Box::new(ValueType::String)))],
            )],
        );
        let graph = import(xml, Some(declared_elsewhere)).expect("import succeeds");
        assert_eq!(
            single_value(&graph, "tags"),
            ElementProperty::from(r#"["a"]"#)
        );
    }

    #[test]
    fn declared_container_with_invalid_json_or_wrong_items_fails() {
        let xml = |text: &str| {
            format!(
                r#"<graphml>
          <key id="tags" for="node" attr.name="tags" attr.type="string"/>
          <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
          <graph><node id="1"><data key="labelV">doc</data><data key="tags">{text}</data></node></graph>
        </graphml>"#
            )
        };
        let schema = || {
            schema_with(
                SchemaMode::None,
                &[(
                    "doc",
                    &[("tags", ValueType::Array(Box::new(ValueType::Int64)))],
                )],
            )
        };
        let err = import(&xml("[1, 2"), Some(schema())).expect_err("invalid JSON is rejected");
        assert!(
            matches!(&err, IOError::InvalidJsonValue { key, declared, .. }
                if key == "tags" && declared == "array<int64>"),
            "got {err:?}"
        );
        // Arbitrarily deep input fails with an error, not a stack overflow.
        let err =
            import(&xml(&"[".repeat(200_000)), Some(schema())).expect_err("deep JSON is rejected");
        assert!(
            matches!(&err, IOError::InvalidJsonValue { reason, .. } if reason.contains("nested too deeply")),
            "got {err:?}"
        );
        let err =
            import(&xml(r#"["a"]"#), Some(schema())).expect_err("wrong item types are rejected");
        assert!(
            matches!(&err, IOError::InvalidAttributeValue { key, expected_type, .. }
                if key == "tags" && expected_type == "array<int64>"),
            "got {err:?}"
        );
    }

    /// A union of a container and a text type cannot be told apart in GraphML text.
    #[test]
    fn a_union_of_a_container_and_a_text_type_is_ambiguous() {
        let xml = r#"<graphml>
          <key id="tags" for="node" attr.name="tags" attr.type="string"/>
          <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
          <graph><node id="1"><data key="labelV">doc</data><data key="tags">["a"]</data></node></graph>
        </graphml>"#;
        for declared in [
            ValueType::Union(vec![
                ValueType::String,
                ValueType::Array(Box::new(ValueType::Int64)),
            ]),
            ValueType::Union(vec![ValueType::Uuid, ValueType::Object]),
        ] {
            let schema = schema_with(SchemaMode::None, &[("doc", &[("tags", declared.clone())])]);
            let err = import(xml, Some(schema)).expect_err("ambiguous declarations are rejected");
            assert!(
                matches!(&err, IOError::AmbiguousContainerImport { key, .. } if key == "tags"),
                "declared {declared}: got {err:?}"
            );
        }
    }

    #[test]
    fn int64_under_a_promoted_string_key_exports_as_its_digits() {
        let graph = GraphSource::empty();
        add_vertex(&graph, "a", vec![("k", 29i64.into())]);
        add_vertex(&graph, "b", vec![("k", "x".into())]);
        let xml = export(&*graph.read());
        assert!(
            xml.contains(r#"<key id="k" for="node" attr.name="k" attr.type="string"/>"#),
            "{xml}"
        );
        assert!(xml.contains(r#"<data key="k">29</data>"#), "{xml}");

        // Undeclared, the digits come back as the string they were written as.
        let imported = import(&xml, None).expect("import succeeds");
        let values = imported
            .read()
            .traversal()
            .v(None::<()>)
            .has_label("a")
            .values("k")
            .to_list()
            .expect("values() succeeds");
        assert_eq!(values, vec![ElementProperty::from("29")]);
    }

    #[test]
    fn non_finite_float_is_an_export_error() {
        let graph = GraphSource::empty();
        add_vertex(
            &graph,
            "m",
            vec![("w", ElementProperty::Number(Number::from_f64(f64::NAN)))],
        );
        let mut buf = Vec::new();
        let err = GraphMLExport::new()
            .export_writer(&*graph.read(), &mut buf)
            .expect_err("NaN cannot be exported");
        assert!(
            matches!(&err, IOError::NonFiniteFloat { key, .. } if key == "w"),
            "{err:?}"
        );
    }

    #[test]
    fn edges_without_stored_ids_export_without_an_id_and_import_back() {
        let mut inner = TraversalGraph::new();
        let a = inner
            .add_vertex(Some("a".into()), vec!["n".into()], PropertyMap::default())
            .unwrap();
        let b = inner
            .add_vertex(Some("b".into()), vec!["n".into()], PropertyMap::default())
            .unwrap();
        let mut props = PropertyMap::default();
        props.insert("w", 0.5f64.into());
        inner
            .add_edge(None, Some("to".into()), a, b, props)
            .unwrap();
        let storage = super::id_less::IdLessEdges { inner };

        let xml = export(&storage);
        assert!(xml.contains(r#"<edge source="a" target="b">"#), "{xml}");
        assert_no_debug_text(&xml);

        let imported = import(&xml, None).expect("an edge without an id imports");
        let lock = imported.read();
        let weights = lock
            .traversal()
            .e(None::<()>)
            .values("w")
            .to_list()
            .unwrap();
        assert_eq!(weights, vec![ElementProperty::from(0.5f64)]);
        let ids = lock.traversal().e(None::<()>).id().to_list().unwrap();
        assert_eq!(ids.len(), 1, "the importing graph assigns an id");
    }

    #[test]
    fn id_of_an_edge_without_a_stored_id_is_an_error_not_debug_text() {
        let mut inner = TraversalGraph::new();
        let a = inner
            .add_vertex(Some("a".into()), vec![], PropertyMap::default())
            .unwrap();
        inner
            .add_edge(None, Some("to".into()), a, a, PropertyMap::default())
            .unwrap();
        let mut storage = super::id_less::IdLessEdges { inner };

        let g = GraphTraversalSource::new(&mut storage);
        let err = g.e(None::<()>).id().to_list().expect_err("no stored id");
        // Located at the step that returned the edge; the cause names the missing id.
        assert!(
            err.root_cause().to_string().contains("edge has no id"),
            "{err}"
        );
    }
}

/// A storage whose edges have no stored id, delegating everything else to a `TraversalGraph`.
mod id_less {
    use compact_str::CompactString;
    use graphersal::error::GraphError;
    use graphersal::prelude::{ElementProperty, PropertyMap};
    use graphersal::{
        prelude::TraversalGraph,
        storage::{EdgeHandle, GraphStorage, VertexHandle},
    };

    pub(super) struct IdLessEdges {
        pub(super) inner: TraversalGraph,
    }

    impl GraphStorage for IdLessEdges {
        type VertexId = VertexHandle;
        type EdgeId = EdgeHandle;

        fn vertex_count(&self) -> usize {
            self.inner.vertex_count()
        }
        fn edge_count(&self) -> usize {
            self.inner.edge_count()
        }
        fn vertices(&self) -> impl Iterator<Item = VertexHandle> + '_ {
            GraphStorage::vertices(&self.inner)
        }
        fn edges(&self) -> impl Iterator<Item = EdgeHandle> + '_ {
            GraphStorage::edges(&self.inner)
        }
        fn get_vertex_by_id(&self, id: &str) -> Option<VertexHandle> {
            self.inner.get_vertex_by_id(id)
        }
        fn get_edge_by_id(&self, _id: &str) -> Option<EdgeHandle> {
            None
        }
        fn out_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
            GraphStorage::out_edges(&self.inner, node)
        }
        fn in_edges(&self, node: VertexHandle) -> impl Iterator<Item = EdgeHandle> + '_ {
            GraphStorage::in_edges(&self.inner, node)
        }
        fn vertex_id(&self, node: VertexHandle) -> Option<&str> {
            self.inner.vertex_id(node)
        }
        fn edge_id(&self, _edge: EdgeHandle) -> Option<&str> {
            None
        }
        fn vertex_label(&self, node: VertexHandle) -> Option<&str> {
            self.inner.vertex_label(node)
        }
        fn edge_label(&self, edge: EdgeHandle) -> Option<&str> {
            self.inner.edge_label(edge)
        }
        fn vertex_labels(&self, node: VertexHandle) -> &[CompactString] {
            self.inner.vertex_labels(node)
        }
        fn vertex_properties(&self, node: VertexHandle) -> Option<&PropertyMap> {
            self.inner.vertex_properties(node)
        }
        fn edge_properties(&self, edge: EdgeHandle) -> Option<&PropertyMap> {
            self.inner.edge_properties(edge)
        }
        fn edge_endpoints(&self, edge: EdgeHandle) -> Option<(VertexHandle, VertexHandle)> {
            self.inner.edge_endpoints(edge)
        }

        fn insert_vertex(
            &mut self,
            id: Option<CompactString>,
            labels: Vec<CompactString>,
            properties: PropertyMap,
        ) -> Result<VertexHandle, GraphError> {
            self.inner.insert_vertex(id, labels, properties)
        }
        fn add_vertex_label(
            &mut self,
            node: VertexHandle,
            label: CompactString,
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
            id: Option<CompactString>,
            label: Option<CompactString>,
            from: VertexHandle,
            to: VertexHandle,
            properties: PropertyMap,
        ) -> Result<EdgeHandle, GraphError> {
            self.inner.insert_edge(id, label, from, to, properties)
        }
        fn set_vertex_property(
            &mut self,
            node: VertexHandle,
            key: CompactString,
            value: ElementProperty,
        ) -> Result<(), GraphError> {
            self.inner.set_vertex_property(node, key, value)
        }
        fn set_edge_property(
            &mut self,
            edge: EdgeHandle,
            key: CompactString,
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
}

/// GraphML carries no schema: an export never contains one, and the save -> load round trip
/// sets the schema on the target FIRST (as the playground and every front end do), then imports.
/// Through the declarations every logical type comes back, so ids, labels, properties and types
/// are unchanged.
#[cfg(feature = "serde")]
mod schema_set_separately {
    use graphersal::io::{GraphImport, GraphMLExport};
    use graphersal::prelude::*;
    use graphersal::schema::{PropertySchema, ValueType};

    const UID: &str = "550e8400-e29b-41d4-a716-446655440000";

    type VertexRow = (String, Vec<String>, PropertyMap);
    type EdgeRow = (String, String, String, String, PropertyMap);

    fn export(graph: &Graph) -> String {
        let mut buf = Vec::new();
        GraphMLExport::new()
            .export_writer(&*graph.read(), &mut buf)
            .expect("export succeeds");
        String::from_utf8(buf).expect("GraphML is UTF-8")
    }

    fn import_into(mut graph: Graph, xml: &str) -> Result<Graph, IOError> {
        graph.import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()))?;
        Ok(graph)
    }

    /// An empty graph that stores `schema`, as the target of an import.
    fn target_with(schema: GraphSchema) -> Graph {
        let graph = GraphSource::empty();
        graph.write().set_schema(schema).expect("schema applies");
        graph
    }

    /// Every vertex (id, labels, properties) and edge (id, label, out, in, properties), sorted.
    fn snapshot(graph: &Graph) -> (Vec<VertexRow>, Vec<EdgeRow>) {
        let guard = graph.read();
        let g: &TraversalGraph = &guard;
        let mut vertices: Vec<VertexRow> = g
            .vertices()
            .map(|v| {
                (
                    g.vertex_id(v).unwrap_or_default().to_string(),
                    g.vertex_labels(v).iter().map(|l| l.to_string()).collect(),
                    g.vertex_properties(v).cloned().unwrap_or_default(),
                )
            })
            .collect();
        vertices.sort_by(|a, b| a.0.cmp(&b.0));
        let mut edges: Vec<EdgeRow> = g
            .edges()
            .map(|e| {
                let (out_v, in_v) = g.edge_endpoints(e).expect("endpoints");
                (
                    g.edge_id(e).unwrap_or_default().to_string(),
                    g.edge_label(e).unwrap_or_default().to_string(),
                    g.vertex_id(out_v).unwrap_or_default().to_string(),
                    g.vertex_id(in_v).unwrap_or_default().to_string(),
                    g.edge_properties(e).cloned().unwrap_or_default(),
                )
            })
            .collect();
        edges.sort_by(|a, b| a.0.cmp(&b.0));
        (vertices, edges)
    }

    fn nullable(ty: ValueType) -> PropertySchema {
        super::codec::node(&ty).nullable()
    }

    /// The modern graph under a Closed schema that also declares a `uuid`, an `array` and an
    /// `object` property on `person`, all three set on marko.
    fn modern_with_schema() -> Graph {
        let graph = GraphSource::tinkerpop_modern();
        let mut schema = graph
            .read()
            .traversal()
            .infer_schema()
            .unwrap()
            .with_mode(SchemaMode::Closed)
            .with_meta(ElementProperty::from("round trip"));
        let person = &mut schema
            .vertices
            .get_mut("person")
            .expect("person label")
            .schema;
        person
            .properties
            .insert("uid".to_string(), nullable(ValueType::Uuid));
        person.properties.insert(
            "tags".to_string(),
            nullable(ValueType::Array(Box::new(ValueType::String))),
        );
        let runtime = PropertySchema::object()
            .with_required_property("cpu", PropertySchema::integer())
            .with_required_property("ratio", PropertySchema::number());
        let config = PropertySchema::object()
            .with_required_property("runtime", runtime)
            .with_required_property("name", PropertySchema::string())
            .nullable();
        person.properties.insert("config".to_string(), config);
        graph.write().set_schema(schema).expect("schema applies");
        let marko = graph.read().get_vertex_by_id("1").expect("marko");
        let mut runtime = PropertyMap::default();
        runtime.insert("cpu", 2i64.into());
        runtime.insert("ratio", 0.5f64.into());
        let mut config = PropertyMap::default();
        config.insert("runtime", ElementProperty::Object(Box::new(runtime)));
        config.insert("name", "a <b> & \"c\"".into());
        for (key, value) in [
            ("uid", ElementProperty::uuid_from_string(UID).expect("uuid")),
            ("tags", ElementProperty::Array(vec!["x".into(), "y".into()])),
            ("config", ElementProperty::Object(Box::new(config))),
        ] {
            graph
                .write()
                .set_vertex_property(marko, key.into(), value)
                .expect("property is set");
        }
        graph
    }

    #[test]
    fn an_export_never_contains_a_schema() {
        let xml = export(&modern_with_schema());
        assert!(!xml.contains("graphersal.schema"), "{xml}");
        assert!(!xml.contains("for=\"graph\""), "{xml}");
        assert!(
            !xml.contains("round trip"),
            "the schema comment is not exported: {xml}"
        );
    }

    #[test]
    fn save_then_load_with_the_schema_set_first_restores_every_type() {
        let original = modern_with_schema();
        let schema = original.read().get_schema().unwrap().expect("schema");
        let xml = export(&original);

        let loaded = import_into(target_with(schema.clone()), &xml).expect("import succeeds");
        assert_eq!(snapshot(&loaded), snapshot(&original));
        assert_eq!(loaded.read().get_schema().unwrap(), Some(schema));
        let uid = loaded
            .read()
            .traversal()
            .v("1")
            .values("uid")
            .to_list()
            .expect("values");
        assert_eq!(uid, vec![ElementProperty::uuid_from_string(UID).unwrap()]);

        // A second round trip is byte-identical: nothing drifts.
        assert_eq!(export(&loaded), xml);
    }

    /// The schema travels as its own JSON text (what the playground saves as `schema.json`).
    #[test]
    fn the_schema_travels_as_its_own_json() {
        let original = modern_with_schema();
        let json = original
            .read()
            .get_schema()
            .unwrap()
            .expect("schema")
            .to_json_pretty();
        let xml = export(&original);
        let schema = GraphSchema::from_json(&json).expect("schema reads back");
        let loaded = import_into(target_with(schema), &xml).expect("import succeeds");
        assert_eq!(snapshot(&loaded), snapshot(&original));
    }

    /// Without a schema on the target, the declared types are not restored: a uuid and the
    /// containers come back as strings, never guessed.
    #[test]
    fn without_the_schema_typed_values_load_as_strings() {
        let original = modern_with_schema();
        let xml = export(&original);
        let loaded = import_into(GraphSource::empty(), &xml).expect("import succeeds");
        assert!(loaded.read().get_schema().unwrap().is_none());
        let values = |key: &str| {
            loaded
                .read()
                .traversal()
                .v("1")
                .values(key)
                .to_list()
                .expect("values")
        };
        assert_eq!(values("uid"), vec![ElementProperty::from(UID)]);
        assert_eq!(values("tags"), vec![ElementProperty::from(r#"["x","y"]"#)]);
        assert!(
            matches!(&values("config")[..], [ElementProperty::String(_)]),
            "{:?}",
            values("config")
        );
    }

    /// A document written by an earlier version with a graph-level `graphersal.schema` value
    /// imports like any other graph-level data: ignored, the target's schema is untouched.
    #[test]
    fn a_legacy_graph_level_schema_value_is_ignored() {
        let xml = r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
              <key id="s" for="graph" attr.name="graphersal.schema" attr.type="string"/>
              <graph><data key="s">{"version":1,"mode":"closed","vertices":{}}</data><node id="1"/></graph>
            </graphml>"#;
        let loaded = import_into(GraphSource::empty(), xml).expect("import succeeds");
        assert!(loaded.read().get_schema().unwrap().is_none());
        assert_eq!(loaded.read().vertex_count(), 1);
        assert!(
            loaded
                .read()
                .vertex_properties(loaded.read().get_vertex_by_id("1").unwrap())
                .is_none_or(|p| p.is_empty())
        );
    }
}
