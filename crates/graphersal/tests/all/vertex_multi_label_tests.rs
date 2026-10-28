//! Integration tests for vertex multi-label storage (design spec,
//! no longer kept). Covers T2-T9 from the spec's Testing
//! section; T1 (backward compatibility) is the rest of the existing suite passing unchanged, and
//! T8 (edges unaffected) is covered by the pre-existing edge-label tests, run unmodified.

use graphersal::prelude::PropertyMap;
use graphersal::prelude::*;
use graphersal::schema::{PropertySchema, VertexSchema};

#[cfg(feature = "script")]
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// T2: basic multi-label creation, `vertex_labels()`, `label()`, `labels()`.
// ---------------------------------------------------------------------------------------------

#[test]
fn t2_add_vertex_with_multiple_labels_rust() {
    let mut tg = TraversalGraph::new();
    let idx = tg
        .add_vertex(
            Some("1".into()),
            vec!["Person".into(), "Admin".into()],
            PropertyMap::default(),
        )
        .unwrap();

    assert_eq!(
        GraphStorage::vertex_labels(&tg, idx).unwrap(),
        &[
            compact_str::CompactString::new("Person"),
            compact_str::CompactString::new("Admin")
        ][..]
    );
    assert_eq!(
        GraphStorage::vertex_label(&tg, idx).unwrap(),
        Some("Person")
    );

    let g = tg.traversal();
    let label = g.v("1").label().to_list().unwrap();
    assert_eq!(label[0], ElementProperty::String("Person".into()));

    let g = tg.traversal();
    let labels = g.v("1").labels().to_list().unwrap();
    assert_eq!(
        labels[0],
        ElementProperty::Array(vec![
            ElementProperty::String("Person".into()),
            ElementProperty::String("Admin".into()),
        ])
    );
}

#[test]
#[cfg(feature = "script")]
fn t2_add_v_with_multiple_labels_rhai() {
    use crate::script_host::eval_value;

    let graph = Arc::new(GraphSource::empty());
    let _ = eval_value(graph.clone(), r#"g.addV(["Person", "Admin"]).toList()"#)
        .expect("addV with a label list must succeed");

    let labels = eval_value(graph.clone(), r#"g.V().labels().toList()[0]"#)
        .expect("labels() evaluation failed");
    let labels: Vec<String> = labels
        .into_array()
        .expect("labels() must return a list")
        .into_iter()
        .map(|d| d.into_string().unwrap())
        .collect();
    assert_eq!(labels, vec!["Person".to_string(), "Admin".to_string()]);

    let label = eval_value(graph.clone(), r#"g.V().label().toList()[0]"#)
        .expect("label() evaluation failed");
    assert_eq!(label.into_string().unwrap(), "Person");
}

// ---------------------------------------------------------------------------------------------
// T3: `has_label` generalization to label-set membership.
// ---------------------------------------------------------------------------------------------

#[test]
fn t3_has_label_membership_on_multi_label_vertex() {
    let mut tg = TraversalGraph::new();
    tg.add_vertex(
        Some("1".into()),
        vec!["Person".into(), "Admin".into()],
        PropertyMap::default(),
    )
    .unwrap();

    let g = tg.traversal();
    assert_eq!(
        g.v(None)
            .has_label("Person")
            .count()
            .next()
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(1)
    );
    let g = tg.traversal();
    assert_eq!(
        g.v(None)
            .has_label("Admin")
            .count()
            .next()
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(1)
    );
    let g = tg.traversal();
    assert_eq!(
        g.v(None)
            .has_label(vec!["Person", "Nonexistent"])
            .count()
            .next()
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(1)
    );
    let g = tg.traversal();
    assert_eq!(
        g.v(None)
            .has_label("Nonexistent")
            .count()
            .next()
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(0)
    );
}

// ---------------------------------------------------------------------------------------------
// T4: D5 pushdown-correctness regressions.
// ---------------------------------------------------------------------------------------------

#[test]
fn t4_bug1_consecutive_has_label_survives_multi_label_vertex() {
    let mut tg = TraversalGraph::new();
    tg.add_vertex(
        Some("1".into()),
        vec!["a".into(), "b".into()],
        PropertyMap::default(),
    )
    .unwrap();

    let g = tg.traversal();
    let result = g
        .v(None)
        .has_label("a")
        .has_label("b")
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(result.as_i64(), Some(1));

    // Same result whether or not the optimizer runs.
    let g = tg.traversal();
    let profiled = g
        .v(None)
        .has_label("a")
        .has_label("b")
        .count()
        .profile()
        .unwrap();
    assert!(profiled.to_string().contains('1'));
}

#[test]
fn t4_bug2_has_label_multi_candidate_count_is_not_doubled() {
    let mut tg = TraversalGraph::new();
    tg.add_vertex(
        Some("1".into()),
        vec!["a".into(), "b".into()],
        PropertyMap::default(),
    )
    .unwrap();

    let g = tg.traversal();
    let result = g
        .v(None)
        .has_label(vec!["a", "b"])
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        result.as_i64(),
        Some(1),
        "a {{a,b}}-tagged vertex must be counted once, not once per matching label"
    );

    // Materialized list must also contain exactly one vertex, not two.
    let g = tg.traversal();
    let list = g.v(None).has_label(vec!["a", "b"]).id().to_list().unwrap();
    assert_eq!(list.len(), 1);
}

#[test]
#[cfg(feature = "script")]
fn t4_bug2_rhai_end_to_end() {
    use crate::script_host::eval_value;

    let graph = Arc::new(GraphSource::empty());
    let _ = eval_value(graph.clone(), r#"g.addV(["a", "b"]).toList()"#)
        .expect("addV with a label list must succeed");

    let count = eval_value(
        graph.clone(),
        r#"g.V().hasLabel("a").hasLabel("b").count().toList()[0]"#,
    )
    .expect("consecutive hasLabel evaluation failed");
    assert_eq!(count.as_int().unwrap(), 1);

    let count2 = eval_value(
        graph.clone(),
        r#"g.V().hasLabel(["a", "b"]).count().toList()[0]"#,
    )
    .expect("multi-candidate hasLabel evaluation failed");
    assert_eq!(count2.as_int().unwrap(), 1);
}

// ---------------------------------------------------------------------------------------------
// T5: storage mutators (`add_vertex_label`/`remove_vertex_label`) and index consistency.
// ---------------------------------------------------------------------------------------------

#[test]
fn t5_add_vertex_label_is_idempotent_and_updates_index() {
    let mut tg = TraversalGraph::new();
    let idx = tg
        .add_vertex(
            Some("1".into()),
            vec!["Person".into()],
            PropertyMap::default(),
        )
        .unwrap();

    assert!(tg.add_vertex_label(idx, "Admin".into()).unwrap());
    assert_eq!(
        GraphStorage::vertex_labels(&tg, idx).unwrap(),
        &[
            compact_str::CompactString::new("Person"),
            compact_str::CompactString::new("Admin")
        ][..]
    );

    // Re-adding the same label is a no-op, not an error.
    assert!(!tg.add_vertex_label(idx, "Admin".into()).unwrap());
    assert_eq!(GraphStorage::vertex_labels(&tg, idx).unwrap().len(), 2);

    // The label index is consistent with the vertex's label vec.
    let g = tg.traversal();
    assert_eq!(
        g.v(None)
            .has_label("Admin")
            .count()
            .next()
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(1)
    );
}

#[test]
fn t5_remove_vertex_label_to_zero_labels_stays_queryable_by_id() {
    let mut tg = TraversalGraph::new();
    let idx = tg
        .add_vertex(
            Some("1".into()),
            vec!["Person".into()],
            PropertyMap::default(),
        )
        .unwrap();

    assert!(tg.remove_vertex_label(idx, "Person").unwrap());
    assert!(GraphStorage::vertex_labels(&tg, idx).unwrap().is_empty());
    assert_eq!(GraphStorage::vertex_label(&tg, idx).unwrap(), None);

    // Removing an absent label is a no-op, not an error.
    assert!(!tg.remove_vertex_label(idx, "Person").unwrap());

    // Still queryable by ID.
    assert!(GraphStorage::get_vertex_by_id(&tg, "1").unwrap().is_some());

    // But absent from every label bucket.
    let g = tg.traversal();
    assert_eq!(
        g.v(None)
            .has_label("Person")
            .count()
            .next()
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(0)
    );
    let g = tg.traversal();
    assert_eq!(g.v(None).count().next().unwrap().unwrap().as_i64(), Some(1));
}

#[test]
fn t5_add_vertex_label_rejects_double_colon() {
    let mut tg = TraversalGraph::new();
    let idx = tg
        .add_vertex(Some("1".into()), vec![], PropertyMap::default())
        .unwrap();

    let err = tg.add_vertex_label(idx, "a::b".into());
    assert!(err.is_err());
}

// ---------------------------------------------------------------------------------------------
// T6: schema inference/validation over multi-label vertices.
// ---------------------------------------------------------------------------------------------

#[test]
fn t6_schema_inference_populates_every_label_bucket() {
    let mut tg = TraversalGraph::new();
    let mut props = PropertyMap::default();
    props.insert("name", ElementProperty::String("alice".into()));
    tg.add_vertex(
        Some("1".into()),
        vec!["Person".into(), "Employee".into()],
        props,
    )
    .unwrap();

    let schema = tg.traversal().infer_schema().unwrap();
    let person = &schema.vertices.get("Person").expect("Person bucket").schema;
    let employee = &schema
        .vertices
        .get("Employee")
        .expect("Employee bucket")
        .schema;
    assert!(person.properties.contains_key("name"));
    assert!(employee.properties.contains_key("name"));
    assert_eq!(person.required, vec!["name"]);
}

#[test]
fn t6_schema_enforced_validates_every_label_independently() {
    let mut schema = GraphSchema::new().with_mode(SchemaMode::Closed);

    schema.vertices.insert(
        "Person".to_string(),
        VertexSchema::new(
            PropertySchema::object().with_required_property("name", PropertySchema::string()),
        ),
    );
    schema.vertices.insert(
        "Employee".to_string(),
        VertexSchema::new(
            PropertySchema::object().with_required_property("badge", PropertySchema::string()),
        ),
    );

    let mut tg = TraversalGraph::new();
    tg.set_schema(schema).unwrap();

    // Missing "badge" (required by the Employee entry) must fail even though "name" (required by
    // the Person entry) is present -- both label entries are validated independently.
    let mut props = PropertyMap::default();
    props.insert("name", ElementProperty::String("alice".into()));
    let err = tg.add_vertex(
        Some("1".into()),
        vec!["Person".into(), "Employee".into()],
        props,
    );
    assert!(err.is_err());

    // Providing both required properties satisfies both entries independently.
    let mut props_ok = PropertyMap::default();
    props_ok.insert("name", ElementProperty::String("bob".into()));
    props_ok.insert("badge", ElementProperty::String("b1".into()));
    let ok = tg.add_vertex(
        Some("2".into()),
        vec!["Person".into(), "Employee".into()],
        props_ok,
    );
    assert!(ok.is_ok());
}

// ---------------------------------------------------------------------------------------------
// T7: GraphML round-trip for single-label, multi-label, and unlabeled vertices; "::" rejection.
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "io")]
mod graphml {
    use super::*;
    use graphersal::io::{GraphExport, GraphImport};

    #[test]
    fn t7_roundtrip_preserves_label_sets_in_order() {
        let graph = GraphSource::empty();
        graph
            .write()
            .traversal_mut()
            .add_v("Person")
            .property("name", "single")
            .next()
            .unwrap();
        graph
            .write()
            .traversal_mut()
            .add_v(vec!["Person", "Admin"])
            .property("name", "multi")
            .next()
            .unwrap();
        graph
            .write()
            .traversal_mut()
            .add_v(None)
            .property("name", "unlabeled")
            .next()
            .unwrap();

        let temp_path = std::env::temp_dir()
            .join("vertex_multi_label_roundtrip_test.graphml")
            .to_str()
            .unwrap()
            .to_string();
        graph
            .export_graphml(&temp_path)
            .expect("export should succeed");

        let mut imported = GraphSource::empty();
        imported
            .import_graphml(&temp_path)
            .expect("import should succeed");
        let _ = std::fs::remove_file(&temp_path);

        let lock = imported.read();
        let single_idx = GraphStorage::get_vertex_by_id(&*lock, "1")
            .unwrap()
            .unwrap();
        let multi_idx = GraphStorage::get_vertex_by_id(&*lock, "2")
            .unwrap()
            .unwrap();
        let unlabeled_idx = GraphStorage::get_vertex_by_id(&*lock, "3")
            .unwrap()
            .unwrap();

        assert_eq!(
            GraphStorage::vertex_labels(&*lock, single_idx).unwrap(),
            &[compact_str::CompactString::new("Person")][..]
        );
        assert_eq!(
            GraphStorage::vertex_labels(&*lock, multi_idx).unwrap(),
            &[
                compact_str::CompactString::new("Person"),
                compact_str::CompactString::new("Admin")
            ][..]
        );
        assert!(
            GraphStorage::vertex_labels(&*lock, unlabeled_idx)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn t7_double_colon_label_is_rejected_at_add_vertex_time() {
        let mut tg = TraversalGraph::new();
        let result = tg.add_vertex(
            Some("1".into()),
            vec!["a::b".into()],
            PropertyMap::default(),
        );
        assert!(
            result.is_err(),
            "a label containing \"::\" must be rejected at add_vertex time"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Acceptance-adjacent: label containing "::" is rejected via add_v() too (D8 + D9 interaction).
// ---------------------------------------------------------------------------------------------

#[test]
fn add_v_rejects_a_double_colon_label() {
    let mut tg = TraversalGraph::new();
    let result = tg.traversal_mut().add_v("a::b").next();
    assert!(result.is_err());
}
