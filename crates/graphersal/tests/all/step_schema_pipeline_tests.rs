//! The `infer_schema()` step: the schema of a stream (the same inference as the source method
//! `g.infer_schema()`), its materialization in the canonical schema shape, its rendering, and
//! the Rhai spellings. The raw stream values of the step are checked by the unit tests of
//! `traversal/step/barrier_steps/step_infer_schema.rs`.
use graphersal::prelude::GraphSource;
use graphersal::prelude::*;
use graphersal::schema::{EdgeConnection, SCHEMA_FORMAT_URL};

/// `to_list()` materializes the schema in its canonical shape (the format), the one shape a
/// schema has as data everywhere.
#[test]
fn test_infer_schema_step_materializes_the_canonical_shape() {
    let graph = GraphSource::tinkerpop_modern();
    let g_lock = graph.read();
    let res = g_lock
        .traversal()
        .e(None)
        .infer_schema_step()
        .to_list()
        .unwrap();
    assert_eq!(res.len(), 1);
    let root = res[0].as_object().expect("a schema map");
    assert_eq!(root.get("$schema"), Some(&SCHEMA_FORMAT_URL.into()));
    assert_eq!(root.get("mode"), Some(&"none".into()));
    let vertices = root.get("vertices").and_then(|v| v.as_object()).unwrap();
    assert!(vertices.is_empty(), "an edge stream describes edges only");
    let schema = GraphSchema::from_value(&res[0]).expect("the format reads back");
    assert_eq!(
        schema.edges["created"].schema.properties["weight"].type_name(),
        "float64"
    );
    assert_eq!(
        schema.edges["created"].connections,
        vec![EdgeConnection::new("person", "software")]
    );
}

#[test]
fn test_infer_schema_step_profile() {
    let graph = GraphSource::tinkerpop_modern();
    let g_lock = graph.read();
    let profile = g_lock
        .traversal()
        .v(None)
        .has_label("person")
        .infer_schema_step()
        .profile()
        .unwrap();
    assert!(profile.to_string().contains("infer_schema()"));
}

#[test]
fn test_traversal_visualize_rust_api() {
    let graph = GraphSource::tinkerpop_modern();
    let g_lock = graph.read();
    let person = || {
        g_lock
            .traversal()
            .v(None)
            .has_label("person")
            .infer_schema_step()
    };
    #[cfg(feature = "display")]
    {
        let table = person().visualize(VisualizeFormat::Table).unwrap();
        assert!(table.contains("person") && table.contains("age"));
        assert_eq!(table, person().to_table().unwrap());
    }
    let markdown = person().to_markdown().unwrap();
    assert!(markdown.contains("### Vertices"));
    assert!(markdown.contains("*(no edge labels)*"));
    assert!(person().to_mermaid().unwrap().starts_with("erDiagram"));
    assert!(person().to_plantuml().unwrap().starts_with("@startuml"));
    let json = person().to_json().unwrap();
    assert!(GraphSchema::from_json(&json).is_ok(), "{json}");
    assert!(
        person()
            .to_json_schema()
            .unwrap()
            .contains("\"vertex:person\"")
    );
    assert!(person().to_tree().unwrap().contains("vertex person"));
}

#[test]
#[cfg(feature = "script")]
fn test_infer_schema_step_rhai_script() {
    use std::sync::Arc;
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    for script in [
        r#"g.V().hasLabel("person").infer_schema().toList()"#,
        r#"g.v().has_label("person").infer_schema().to_list()"#,
    ] {
        let res = crate::script_host::eval(graph.clone(), script).expect(script);
        assert!(res.contains("vertices") && res.contains("person"), "{res}");
    }
    #[cfg(feature = "display")]
    {
        let table = crate::script_host::eval(
            graph.clone(),
            r#"g.V().hasLabel("person").infer_schema().to_table()"#,
        )
        .unwrap();
        let vis = crate::script_host::eval(
            graph.clone(),
            r#"g.V().hasLabel("person").infer_schema().visualize(V.Table)"#,
        )
        .unwrap();
        assert_eq!(table, vis);
    }
    let md = crate::script_host::eval(
        graph.clone(),
        r#"g.V().hasLabel("person").infer_schema().to_markdown()"#,
    )
    .unwrap();
    assert!(md.contains("### Vertices"));
    let mermaid = crate::script_host::eval(
        graph.clone(),
        r#"g.V().hasLabel("person").infer_schema().to_mermaid()"#,
    )
    .unwrap();
    assert!(mermaid.starts_with("erDiagram"));
    // As a child traversal: `__.infer_schema()`.
    let res = crate::script_host::eval(
        graph.clone(),
        r#"g.V().hasLabel("person").union(__.identity(), __.infer_schema()).count().next()"#,
    )
    .unwrap();
    assert_eq!(res.trim(), "5");
}
