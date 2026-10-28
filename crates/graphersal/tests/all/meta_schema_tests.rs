//! The schema format's meta-schema (`META_SCHEMA_JSON`, `graphersal-schema-v1.json`) and the
//! sample schema file `tests/data/sample_ecommerce_schema.json`: the file loads (the library
//! reads strings, the caller reads the file) and is enforced.
use graphersal::prelude::*;
#[cfg(feature = "serde")]
use graphersal::schema::SCHEMA_FORMAT_URL;
use graphersal::schema::{EdgeConnection, META_SCHEMA_JSON, SchemaPattern};
use std::path::PathBuf;

fn sample_schema_text() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/sample_ecommerce_schema.json");
    std::fs::read_to_string(path).expect("the sample schema file must be readable")
}

/// The keys an object of the meta-schema declares under `properties`, by JSON Pointer-ish
/// path of nested `properties`/`$defs` names.
#[cfg(feature = "serde")]
fn declared_keys(meta: &serde_json::Value, path: &[&str]) -> Vec<String> {
    let mut node = meta;
    for segment in path {
        node = &node[*segment];
    }
    node["properties"]
        .as_object()
        .expect("an object with properties")
        .keys()
        .cloned()
        .collect()
}

#[cfg(feature = "serde")]
#[test]
fn test_meta_schema_constant_validity() {
    let parsed: serde_json::Value =
        serde_json::from_str(META_SCHEMA_JSON).expect("META_SCHEMA_JSON must be valid JSON");
    assert_eq!(parsed["title"], "GraphersalGraphSchema");
    assert_eq!(
        parsed["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(parsed["$id"], SCHEMA_FORMAT_URL);
    assert_eq!(parsed["properties"]["$schema"]["const"], SCHEMA_FORMAT_URL);
    assert_eq!(parsed["additionalProperties"], false);

    // The envelope keys are exactly the ones the loader accepts.
    let mut envelope = declared_keys(&parsed, &[]);
    envelope.sort();
    assert_eq!(envelope, ["$schema", "edges", "meta", "mode", "vertices"]);
    let modes = parsed["properties"]["mode"]["enum"].as_array().unwrap();
    assert_eq!(modes, &vec!["none", "open", "closed"]);

    // The node keywords are exactly the ones the loader accepts: each one loads, and so the
    // meta-schema and the loader cannot drift apart silently.
    let node_keys = declared_keys(&parsed, &["$defs", "node"]);
    for keyword in [
        "type",
        "format",
        "items",
        "properties",
        "required",
        "additionalProperties",
        "anyOf",
        "enum",
        "const",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "formatMinimum",
        "formatMaximum",
        "formatExclusiveMinimum",
        "formatExclusiveMaximum",
        "minLength",
        "maxLength",
        "pattern",
        "minItems",
        "maxItems",
        "title",
        "description",
        "$comment",
        "examples",
        "deprecated",
        "default",
        "defaultFunction",
        "element",
        "dimension",
        "metric",
        "embedder",
        "$ref",
    ] {
        assert!(node_keys.iter().any(|k| k == keyword), "{keyword} missing");
    }
    assert_eq!(node_keys.len(), 34, "{node_keys:?}");
    for rejected in ["oneOf", "allOf", "uniqueItems", "multipleOf", "nullable"] {
        assert!(!node_keys.iter().any(|k| k == rejected), "{rejected}");
    }
    // The formats are uuid, the date/time types and vector.
    assert_eq!(
        parsed["$defs"]["node"]["properties"]["format"]["enum"],
        serde_json::json!([
            "uuid",
            "date-time",
            "date",
            "local-date-time",
            "time",
            "duration",
            "vector"
        ])
    );
    // `default` is documented as applied on create only, never on read or to existing data.
    let default = parsed["$defs"]["node"]["properties"]["default"]["description"]
        .as_str()
        .unwrap();
    assert!(
        default.contains("stored when a vertex or edge is created without the property"),
        "{default}"
    );
    assert!(
        default.contains("never applied on read or to existing data"),
        "{default}"
    );
    assert_eq!(
        parsed["$defs"]["node"]["properties"]["defaultFunction"]["enum"],
        serde_json::json!(["uuidv4", "uuidv7", "now", "today"])
    );
}

#[test]
fn test_meta_schema_is_json_without_serde() {
    // The constant is shipped with every feature set; the loader needs no serde either.
    assert!(META_SCHEMA_JSON.contains("\"$id\": \"https://graphersal.dev/schema/v1\""));
    assert!(GraphSchema::from_json(&sample_schema_text()).is_ok());
}

#[test]
fn test_load_ecommerce_schema_from_text() {
    let schema = GraphSchema::from_json(&sample_schema_text()).expect("the sample schema loads");
    assert_eq!(schema.mode, SchemaMode::Closed);
    let meta = schema.meta.as_ref().expect("meta is kept");
    assert_eq!(
        meta.as_object().unwrap()["title"],
        ElementProperty::from("E-Commerce domain graph schema")
    );

    assert_eq!(schema.vertices.len(), 3);
    let user = &schema.vertices["User"].schema;
    assert_eq!(
        user.annotations().and_then(|a| a.description.as_deref()),
        Some("Registered customer")
    );
    assert_eq!(user.properties.len(), 6);
    assert_eq!(user.required.len(), 6);
    assert_eq!(user.properties["id"].type_name(), "int64");
    assert_eq!(
        user.properties["id"].constraints().unwrap().minimum,
        Some(Number::from_i64(1))
    );
    assert_eq!(
        user.properties["name"]
            .constraints()
            .unwrap()
            .pattern
            .as_ref()
            .map(SchemaPattern::as_str),
        Some("^[a-zA-Z ]+$")
    );
    let address = &user.properties["address"];
    assert_eq!(address.type_name(), "object");
    assert_eq!(address.properties.len(), 3);
    assert_eq!(user.properties["tags"].type_name(), "array<string>");

    assert_eq!(schema.edges.len(), 2);
    let placed = &schema.edges["PLACED"];
    assert_eq!(
        placed.connections,
        vec![EdgeConnection::new("User", "Order")]
    );
    let contains = &schema.edges["CONTAINS"];
    assert_eq!(
        contains.schema.properties["quantity"]
            .constraints()
            .unwrap()
            .minimum,
        Some(Number::from_i64(1))
    );

    // The canonical form round-trips.
    assert_eq!(GraphSchema::from_json(&schema.to_json()).unwrap(), schema);
}

#[test]
fn test_load_schema_invalid_json_fails() {
    let err = GraphSchema::from_json("{ invalid_json: true ").unwrap_err();
    match &err {
        SchemaError::InvalidSchema { path, reason, .. } => {
            assert_eq!(path, "/");
            assert!(reason.starts_with("not valid JSON"), "{reason}");
        }
        other => panic!("Expected SchemaError::InvalidSchema, got {other:?}"),
    }
    assert!(err.help().unwrap().contains("JSON Schema 2020-12"));
}

#[test]
fn test_traversal_graph_and_source_set_schema_and_enforcement() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .set_schema(GraphSchema::from_json(&sample_schema_text()).unwrap())
        .expect("an empty graph satisfies the schema");
    assert_eq!(
        graph.get_schema().unwrap().unwrap().mode,
        SchemaMode::Closed
    );

    let mut user_props = PropertyMap::default();
    user_props.insert("id", ElementProperty::Number(1.into()));
    user_props.insert("name", ElementProperty::String("Alice Smith".into()));
    user_props.insert("email", ElementProperty::String("alice@example.com".into()));
    user_props.insert(
        "created_at",
        ElementProperty::String("2026-09-20T10:00:00Z".into()),
    );
    let mut address_map = PropertyMap::default();
    address_map.insert("street", ElementProperty::String("123 Main St".into()));
    address_map.insert("city", ElementProperty::String("Metropolis".into()));
    address_map.insert("zip", ElementProperty::String("12345".into()));
    user_props.insert("address", ElementProperty::Object(Box::new(address_map)));
    user_props.insert(
        "tags",
        ElementProperty::Array(vec![ElementProperty::String("vip".into())]),
    );
    let u1 = graph
        .add_vertex(Some("u1".into()), vec!["User".into()], user_props)
        .expect("Adding valid User vertex should succeed");

    let mut order_props = PropertyMap::default();
    order_props.insert("order_id", ElementProperty::String("ord-999".into()));
    order_props.insert("total_amount", ElementProperty::Number(149.99.into()));
    order_props.insert("status", ElementProperty::String("PENDING".into()));
    let o1 = graph
        .add_vertex(Some("o1".into()), vec!["Order".into()], order_props.clone())
        .expect("Adding valid Order vertex should succeed");

    // An enum value outside the list is rejected.
    order_props.insert("status", ElementProperty::String("LOST".into()));
    assert!(
        graph
            .add_vertex(Some("o2".into()), vec!["Order".into()], order_props)
            .is_err()
    );

    let mut placed_props = PropertyMap::default();
    placed_props.insert(
        "timestamp",
        ElementProperty::String("2026-09-20T10:05:00Z".into()),
    );
    assert!(
        graph
            .add_edge(
                Some("e1".into()),
                Some("PLACED".into()),
                u1,
                o1,
                placed_props.clone()
            )
            .is_ok()
    );
    // Illegal topology: PLACED from Order to User.
    assert!(
        graph
            .add_edge(
                Some("e2".into()),
                Some("PLACED".into()),
                o1,
                u1,
                placed_props
            )
            .is_err()
    );
    // An undeclared property in closed mode.
    let mut bad_user_props = PropertyMap::default();
    bad_user_props.insert("id", ElementProperty::Number(2.into()));
    bad_user_props.insert(
        "unregistered_extra_field",
        ElementProperty::String("bad".into()),
    );
    assert!(
        graph
            .add_vertex(Some("u2".into()), vec!["User".into()], bad_user_props)
            .is_err()
    );
}

#[test]
fn test_mode_override_through_the_schema_value() {
    let mut graph = TraversalGraph::new();
    let schema = GraphSchema::from_json(&sample_schema_text())
        .unwrap()
        .with_mode(SchemaMode::Open);
    graph.traversal_mut().set_schema(schema).unwrap();
    assert_eq!(
        graph.traversal().get_schema().unwrap().unwrap().mode,
        SchemaMode::Open
    );
    // Open: an undeclared label is fine, a declared one is still typed.
    let ok = graph
        .traversal_mut()
        .add_v("Alien")
        .property("x", 1)
        .to_list();
    assert!(ok.is_ok());
    let bad = graph
        .traversal_mut()
        .add_v("Product")
        .property("sku", "s")
        .property("price", "free")
        .property("category", "c")
        .to_list();
    assert!(bad.is_err());
}
