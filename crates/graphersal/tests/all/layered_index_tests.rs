//! The property-index overlays of a `LayeredGraph` (S4.1): after every write kind, and after
//! the rollback of a unit and of a nested savepoint, every overlay equals a rebuild from
//! scratch and the visible entries (base minus the excluded slots, plus the delta part) equal
//! an index over the layer's elements (`LayeredGraph::check_layer_indexes`). The base carries
//! several indexes: per label and label-less, on vertices and edges, with the `to_lower` and
//! `each` transforms, and on an object-valued key (a skipped value). The base's own indexes are
//! never changed.

use std::sync::Arc;

use graphersal::catalog::{Definition, PropertyIndexDefinition, PropertyIndexTransform};
use graphersal::prelude::*;
use graphersal::storage::{
    ElementKind, GraphStorage, JsonPath, LayeredEdge, LayeredGraph, LayeredVertex, RollbackReason,
};

/// The modern graph with array `tags` and object `meta` on the people, and seven indexes.
fn indexed_base() -> Arc<TraversalGraph> {
    let mut base = TraversalGraph::tinkerpop_modern();
    for (id, tags) in [
        ("1", vec!["a", "b"]),
        ("2", vec!["b"]),
        ("4", vec!["c", "a"]),
    ] {
        let node = base.get_vertex_by_id(id).unwrap().unwrap();
        let tags = ElementProperty::Array(tags.into_iter().map(ElementProperty::from).collect());
        base.set_vertex_property(node, "tags".into(), tags).unwrap();
        let mut meta = PropertyMap::default();
        meta.insert("city", ElementProperty::from("Berlin"));
        base.set_vertex_property(node, "meta".into(), ElementProperty::Object(Box::new(meta)))
            .unwrap();
    }
    let definitions = [
        (
            "age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        ),
        (
            "lang",
            PropertyIndexDefinition::new(ElementKind::Vertex, "software", "lang"),
        ),
        (
            "name",
            PropertyIndexDefinition::label_less(ElementKind::Vertex, "name"),
        ),
        (
            "name_lower",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name")
                .with_transform(Some(PropertyIndexTransform::ToLower)),
        ),
        (
            "tags",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "tags")
                .with_transform(Some(PropertyIndexTransform::Each)),
        ),
        (
            "meta",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "meta"),
        ),
        (
            "knows_weight",
            PropertyIndexDefinition::new(ElementKind::Edge, "knows", "weight"),
        ),
        (
            "weight",
            PropertyIndexDefinition::label_less(ElementKind::Edge, "weight"),
        ),
    ];
    for (name, definition) in definitions {
        base.set_definition(Definition::property_index(name, definition))
            .unwrap();
    }
    base.check_property_indexes().unwrap();
    assert_eq!(base.property_indexes().unwrap().len(), 8);
    Arc::new(base)
}

fn layer() -> LayeredGraph {
    LayeredGraph::over(indexed_base()).unwrap()
}

fn v(layer: &LayeredGraph, id: &str) -> LayeredVertex {
    layer.get_vertex_by_id(id).unwrap().unwrap()
}

fn e(layer: &LayeredGraph, id: &str) -> LayeredEdge {
    layer.get_edge_by_id(id).unwrap().unwrap()
}

fn props(pairs: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in pairs {
        map.insert(*key, value.clone());
    }
    map
}

fn path(text: &str) -> JsonPath {
    text.parse().unwrap()
}

/// The overlays are exact, and the base's indexes are untouched.
#[track_caller]
fn check(layer: &LayeredGraph) {
    if let Err(problem) = layer.check_layer_indexes() {
        panic!("{problem}");
    }
    layer.base().check_property_indexes().unwrap();
}

/// The layer's own index bytes: the overlays' masked and delta parts.
fn overlay_bytes(layer: &LayeredGraph) -> u64 {
    layer.memory_usage().unwrap().unwrap().property_index_bytes
}

#[test]
fn an_untouched_layer_owns_no_index_bytes() {
    let layer = layer();
    check(&layer);
    assert_eq!(overlay_bytes(&layer), 0);
    let stats = layer.memory_usage().unwrap().unwrap();
    let base = layer.base().memory_usage().unwrap().unwrap();
    assert!(base.property_index_bytes > 0);
    assert_eq!(stats.shared_bytes, base.total_bytes());
}

#[test]
fn added_elements_enter_and_leave_the_delta_parts() {
    let mut layer = layer();
    let ada = layer
        .insert_vertex(
            None,
            vec!["person".into()],
            props(&[
                ("name", "Ada".into()),
                ("age", 36.into()),
                (
                    "tags",
                    ElementProperty::Array(vec!["a".into(), "a".into(), "z".into()]),
                ),
            ]),
        )
        .unwrap();
    check(&layer);
    let unlabeled = layer
        .insert_vertex(None, vec![], props(&[("name", "nobody".into())]))
        .unwrap();
    check(&layer);
    let marko = v(&layer, "1");
    // Added edges: added-added, base-added (a cross edge at a stub), base-base, a self-loop.
    let k1 = layer
        .insert_edge(
            None,
            Some("knows".into()),
            ada,
            unlabeled,
            props(&[("weight", 0.1.into())]),
        )
        .unwrap();
    let k2 = layer
        .insert_edge(
            None,
            Some("knows".into()),
            marko,
            ada,
            props(&[("weight", 0.9.into())]),
        )
        .unwrap();
    let k3 = layer
        .insert_edge(
            None,
            Some("likes".into()),
            marko,
            v(&layer, "6"),
            props(&[("weight", 2.into())]),
        )
        .unwrap();
    layer
        .insert_edge(None, None, ada, ada, props(&[("weight", 3.into())]))
        .unwrap();
    check(&layer);
    assert!(overlay_bytes(&layer) > 0);
    layer.drop_edge(k1).unwrap();
    check(&layer);
    layer.drop_edge(k3).unwrap();
    check(&layer);
    // Dropping an added vertex drops its edges (the cross edge k2 and the self-loop).
    layer.drop_vertex(ada).unwrap();
    check(&layer);
    assert!(!layer.contains_edge(k2).unwrap());
    layer.drop_vertex(unlabeled).unwrap();
    check(&layer);
    assert_eq!(overlay_bytes(&layer), 0, "nothing added is left");
}

#[test]
fn property_writes_to_added_elements() {
    let mut layer = layer();
    let ada = layer
        .insert_vertex(
            None,
            vec!["person".into()],
            props(&[("name", "Ada".into())]),
        )
        .unwrap();
    let edge = layer
        .insert_edge(
            None,
            Some("knows".into()),
            ada,
            v(&layer, "2"),
            PropertyMap::default(),
        )
        .unwrap();
    layer
        .set_vertex_property(ada, "age".into(), 36.into())
        .unwrap();
    layer
        .set_vertex_property(ada, "age".into(), 37.into())
        .unwrap();
    layer
        .set_vertex_property(ada, "name".into(), "ADA".into())
        .unwrap();
    layer
        .set_vertex_property(ada, "other".into(), 1.into())
        .unwrap();
    check(&layer);
    layer
        .set_edge_property(edge, "weight".into(), 0.3.into())
        .unwrap();
    check(&layer);
    layer.drop_vertex_property("age", ada).unwrap();
    layer.drop_edge_property("weight", edge).unwrap();
    check(&layer);
    // jpath writes: the whole top-level value changes in the indexes on that key.
    layer
        .set_vertex_property_path(ada, &path("meta.city"), "Paris".into())
        .unwrap();
    check(&layer);
    layer
        .set_vertex_property_path(ada, &path("tags"), ElementProperty::Array(vec!["q".into()]))
        .unwrap();
    layer
        .set_vertex_property_path(ada, &path("tags[1]"), "r".into())
        .unwrap();
    check(&layer);
    layer
        .remove_vertex_property_path(ada, &path("tags[0]"))
        .unwrap();
    layer
        .remove_vertex_property_path(ada, &path("meta"))
        .unwrap();
    check(&layer);
}

#[test]
fn label_changes_of_added_elements() {
    let mut layer = layer();
    let x = layer
        .insert_vertex(
            None,
            vec!["software".into()],
            props(&[
                ("name", "X".into()),
                ("age", 3.into()),
                ("lang", "rust".into()),
            ]),
        )
        .unwrap();
    layer.add_vertex_label(x, "person".into()).unwrap();
    check(&layer);
    layer.remove_vertex_label(x, "software").unwrap();
    check(&layer);
    let edge = layer
        .insert_edge(
            None,
            Some("created".into()),
            x,
            v(&layer, "3"),
            props(&[("weight", 0.7.into())]),
        )
        .unwrap();
    layer.set_edge_label(edge, "knows".into()).unwrap();
    check(&layer);
    layer.set_edge_label(edge, "other".into()).unwrap();
    check(&layer);
}

#[test]
fn writes_to_base_elements_copy_up_into_the_overlays() {
    let mut layer = layer();
    let marko = v(&layer, "1");
    let lop = v(&layer, "3");
    // An identical value copies nothing up and masks nothing.
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    check(&layer);
    assert_eq!(overlay_bytes(&layer), 0);
    // A property write: masked + delta, per label and label-less.
    layer
        .set_vertex_property(marko, "age".into(), 30.into())
        .unwrap();
    check(&layer);
    layer
        .set_vertex_property(marko, "name".into(), "MARKO".into())
        .unwrap();
    check(&layer);
    layer.drop_vertex_property("name", marko).unwrap();
    check(&layer);
    // jpath writes into the `each` and the object-valued keys.
    layer
        .set_vertex_property_path(marko, &path("tags[2]"), "c".into())
        .unwrap();
    check(&layer);
    layer
        .set_vertex_property_path(marko, &path("meta.city"), "Rome".into())
        .unwrap();
    check(&layer);
    layer
        .remove_vertex_property_path(marko, &path("meta"))
        .unwrap();
    check(&layer);
    // Labels of base vertices: gained, lost, re-gained.
    layer.add_vertex_label(lop, "person".into()).unwrap();
    check(&layer);
    layer.remove_vertex_label(lop, "software").unwrap();
    check(&layer);
    layer.remove_vertex_label(marko, "person").unwrap();
    check(&layer);
    layer.add_vertex_label(marko, "person".into()).unwrap();
    check(&layer);
    // Base edges: a property write and relabels, back to the base label too.
    let knows = e(&layer, "0");
    let created = e(&layer, "1");
    layer
        .set_edge_property(knows, "weight".into(), 0.75.into())
        .unwrap();
    check(&layer);
    layer.set_edge_label(created, "knows".into()).unwrap();
    check(&layer);
    layer.set_edge_label(knows, "met".into()).unwrap();
    check(&layer);
    layer.set_edge_label(knows, "knows".into()).unwrap();
    check(&layer);
    layer.drop_edge_property("weight", created).unwrap();
    check(&layer);
}

#[test]
fn a_base_element_changed_back_returns_to_clean() {
    let mut layer = layer();
    let marko = v(&layer, "1");
    let knows = e(&layer, "0");
    layer
        .set_vertex_property(marko, "age".into(), 31.into())
        .unwrap();
    layer.add_vertex_label(marko, "x".into()).unwrap();
    layer
        .set_edge_property(knows, "weight".into(), 0.6.into())
        .unwrap();
    layer.set_edge_label(knows, "met".into()).unwrap();
    check(&layer);
    assert!(overlay_bytes(&layer) > 0);
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    check(&layer);
    layer.remove_vertex_label(marko, "x").unwrap();
    check(&layer);
    layer.set_edge_label(knows, "knows".into()).unwrap();
    check(&layer);
    layer
        .set_edge_property(knows, "weight".into(), 0.5.into())
        .unwrap();
    check(&layer);
    assert_eq!(
        overlay_bytes(&layer),
        0,
        "every image dropped, nothing masked"
    );
}

#[test]
fn dropping_base_elements_masks_them() {
    let mut layer = layer();
    let marko = v(&layer, "1");
    let josh = v(&layer, "4");
    let peter = v(&layer, "6");
    // An imaged base vertex with an added cross edge and imaged base edges, then dropped with
    // its cascade (base edges and the added edges at its stub).
    layer
        .set_vertex_property(marko, "age".into(), 40.into())
        .unwrap();
    layer
        .set_edge_property(e(&layer, "2"), "weight".into(), 0.8.into())
        .unwrap();
    layer
        .insert_edge(
            None,
            Some("knows".into()),
            peter,
            marko,
            props(&[("weight", 0.2.into())]),
        )
        .unwrap();
    check(&layer);
    layer.drop_vertex(marko).unwrap();
    check(&layer);
    // A clean base vertex with clean base edges.
    layer.drop_vertex(josh).unwrap();
    check(&layer);
    // A base edge alone, imaged first.
    let created = e(&layer, "5");
    layer.set_edge_label(created, "knows".into()).unwrap();
    layer.drop_edge(created).unwrap();
    check(&layer);
    // A dropped base id reused by an added vertex.
    layer
        .insert_vertex(
            Some("1".into()),
            vec!["person".into()],
            props(&[("age", 29.into()), ("name", "marko".into())]),
        )
        .unwrap();
    check(&layer);
}

#[test]
fn a_rollback_restores_the_overlays_exactly() {
    let mut layer = layer();
    // Some committed state first.
    let marko = v(&layer, "1");
    layer
        .set_vertex_property(marko, "age".into(), 33.into())
        .unwrap();
    let ada = layer
        .insert_vertex(
            None,
            vec!["person".into()],
            props(&[("age", 20.into()), ("name", "Ada".into())]),
        )
        .unwrap();
    // Images made before the unit, dropped inside it: the undo puts them back.
    let peter = v(&layer, "6");
    layer
        .set_vertex_property(peter, "age".into(), 36.into())
        .unwrap();
    layer.add_vertex_label(peter, "software".into()).unwrap();
    layer
        .set_vertex_property(peter, "lang".into(), "go".into())
        .unwrap();
    let created = e(&layer, "4");
    layer
        .set_edge_property(created, "weight".into(), 0.9.into())
        .unwrap();
    layer.set_edge_label(created, "knows".into()).unwrap();
    check(&layer);
    let before = layer.check_layer_indexes();
    assert!(before.is_ok());
    let bytes = overlay_bytes(&layer);

    let mark = layer.begin_unit().unwrap();
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap(); // back to clean
    layer
        .set_vertex_property(v(&layer, "2"), "age".into(), 1.into())
        .unwrap();
    layer
        .set_vertex_property(ada, "age".into(), 21.into())
        .unwrap();
    layer.add_vertex_label(ada, "software".into()).unwrap();
    layer
        .set_vertex_property_path(v(&layer, "4"), &path("tags[0]"), "zz".into())
        .unwrap();
    layer.set_edge_label(e(&layer, "0"), "met".into()).unwrap();
    layer
        .set_edge_property(e(&layer, "3"), "weight".into(), 9.into())
        .unwrap();
    layer
        .insert_edge(
            None,
            Some("knows".into()),
            ada,
            marko,
            props(&[("weight", 0.4.into())]),
        )
        .unwrap();
    layer.drop_vertex(v(&layer, "4")).unwrap();
    layer.drop_vertex(ada).unwrap();
    layer.drop_vertex(peter).unwrap();
    check(&layer);
    layer
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    check(&layer);
    assert_eq!(overlay_bytes(&layer), bytes);
    assert_eq!(
        layer.vertex_properties(marko).unwrap().unwrap().get("age"),
        Some(&33.into())
    );
}

#[test]
fn a_nested_savepoint_rolls_back_its_part_only() {
    let mut layer = layer();
    let marko = v(&layer, "1");
    let outer = layer.begin_unit().unwrap();
    layer
        .set_vertex_property(marko, "age".into(), 50.into())
        .unwrap();
    let ada = layer
        .insert_vertex(None, vec!["person".into()], props(&[("age", 20.into())]))
        .unwrap();
    check(&layer);
    let bytes = overlay_bytes(&layer);
    let inner = layer.begin_unit().unwrap();
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    layer.drop_vertex(ada).unwrap();
    layer.drop_vertex(v(&layer, "2")).unwrap();
    layer.set_edge_label(e(&layer, "2"), "met".into()).unwrap();
    check(&layer);
    layer
        .rollback_unit(inner, &RollbackReason::failed(&"inner"))
        .unwrap();
    check(&layer);
    assert_eq!(overlay_bytes(&layer), bytes);
    layer.commit_unit(outer).unwrap();
    check(&layer);
    assert_eq!(
        layer.vertex_properties(marko).unwrap().unwrap().get("age"),
        Some(&50.into())
    );
}

/// The Rhai DSL writes go through the same choke points.
#[cfg(feature = "script")]
#[test]
fn dsl_writes_keep_the_overlays_exact() {
    use graphersal::storage::Graph;
    let graph = Arc::new(Graph::new(layer()));
    let scripts = [
        r#"g.v().has_label("person").property("age", 1).iterate()"#,
        r#"g.add_v("person").property("name", "Zed").property("age", 2).iterate()"#,
        r#"g.v().has("name", "lop").drop().iterate()"#,
        r#"g.e().has_label("knows").property("weight", 0.0).iterate()"#,
        r#"g.v().has("name", "Zed").add_e("knows").to(__.v().has("name", "vadas")).property("weight", 5).iterate()"#,
    ];

    for script in scripts {
        let _value = crate::script_host::eval_value_with_limits(
            Arc::clone(&graph),
            script,
            std::collections::HashMap::new(),
            &crate::script_host::ScriptLimits::default(),
        )
        .unwrap();
        check(&graph.read());
    }
}

/// The footprint of the overlays of a layer that changes 1 % of a 20 000-vertex base with
/// three vertex indexes (per label, label-less, a second label): proportional to the touched
/// elements, a small fraction of the base's index bytes, all of it the layer's own.
#[test]
fn a_one_percent_layer_owns_a_small_overlay() {
    let n: usize = std::env::var("LAYERED_INDEX_N")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(20_000);
    let mut base = TraversalGraph::new();
    let mut handles = Vec::with_capacity(n);
    for i in 0..n {
        let label = if i % 2 == 0 { "person" } else { "thing" };
        let node = base
            .insert_vertex(
                None,
                vec![label.into()],
                props(&[
                    ("age", ((i % 100) as i64).into()),
                    ("name", format!("n{i}").into()),
                ]),
            )
            .unwrap();
        handles.push(node);
    }
    for (name, definition) in [
        (
            "age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        ),
        (
            "thing_age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "thing", "age"),
        ),
        (
            "name",
            PropertyIndexDefinition::label_less(ElementKind::Vertex, "name"),
        ),
    ] {
        base.set_definition(Definition::property_index(name, definition))
            .unwrap();
    }
    let base = Arc::new(base);
    let base_index_bytes = base.memory_usage().unwrap().unwrap().property_index_bytes;
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let touched = n / 100;
    let ids: Vec<String> = (0..touched)
        .map(|i| {
            GraphStorage::vertex_id(&*base, handles[i * 100])
                .unwrap()
                .unwrap()
                .to_string()
        })
        .collect();
    // 1 %: half modified (age), a quarter dropped, a quarter added.
    for (i, id) in ids.iter().enumerate() {
        let node = v(&layer, id);
        match i % 4 {
            0 | 1 => layer
                .set_vertex_property(node, "age".into(), 1000.into())
                .unwrap(),
            2 => layer.drop_vertex(node).unwrap(),
            _ => {
                layer
                    .insert_vertex(
                        None,
                        vec!["person".into()],
                        props(&[("age", 7.into()), ("name", format!("new{i}").into())]),
                    )
                    .unwrap();
            }
        }
    }
    check(&layer);
    let stats = layer.memory_usage().unwrap().unwrap();
    let owned = stats.property_index_bytes;
    eprintln!(
        "layered_index footprint: base {n} vertices, {base_index_bytes} B of base index; \
         {touched} touched: overlay {owned} B ({:.2} % of the base index), {:.1} B per touched \
         element; layer total {} B, shared {} B",
        owned as f64 * 100.0 / base_index_bytes as f64,
        owned as f64 / touched as f64,
        stats.total_bytes(),
        stats.shared_bytes,
    );
    assert!(owned > 0);
    assert!(
        owned * 10 < base_index_bytes,
        "{owned} vs {base_index_bytes}"
    );
    assert!(stats.total_bytes() >= owned);
    assert_eq!(
        stats.shared_bytes,
        base.memory_usage().unwrap().unwrap().total_bytes()
    );
}
