//! The property-index overlays of a `LayeredGraph` (S4.1): after every write kind, and after
//! the rollback of a unit and of a nested savepoint, every overlay equals a rebuild from
//! scratch and the visible entries (base minus the excluded slots, plus the delta part) equal
//! an index over the layer's elements (`LayeredGraph::check_layer_indexes`). The base carries
//! several indexes: per label and label-less, on vertices and edges, with the `to_lower` and
//! `each` transforms, and on an object-valued key (a skipped value). The base's own indexes are
//! never changed.
//!
//! The reads (S4.2): after every write kind and after a rollback, every lookup (equality and
//! range probes over every index), every count and the stats of `property_indexes()` equal
//! those of a `TraversalGraph` built with the layer's data (`materialize`), lookups in the same
//! order; `.profile()` on a touched layer shows the index, and the storage answers it.

use std::collections::HashMap;
use std::ops::Bound;
use std::sync::Arc;

use graphersal::catalog::{Definition, PropertyIndexDefinition, PropertyIndexTransform};
use graphersal::prelude::*;
use graphersal::storage::{
    ElementKind, GraphStorage, JsonPath, LayeredEdge, LayeredGraph, LayeredVertex,
    PropertyIndexInfo, PropertyIndexRange, RollbackReason,
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

// ---------------------------------------------------------------------------------------------
// Reads from the overlays (S4.2)
// ---------------------------------------------------------------------------------------------

/// A `TraversalGraph` with the layer's data (every vertex, then every edge, in the layer's scan
/// order, ids, labels and properties as stored) and the base's index definitions: what the
/// layer's lookups must answer like.
pub(crate) fn materialize(layer: &LayeredGraph) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let mut vertices = HashMap::new();
    for node in layer.vertices().unwrap() {
        let id = layer.vertex_id(node).unwrap().map(Into::into);
        let labels = layer.vertex_labels(node).unwrap().to_vec();
        let properties = layer.vertex_properties(node).unwrap().cloned();
        let copy = graph
            .insert_vertex(id, labels, properties.unwrap_or_default())
            .unwrap();
        vertices.insert(node, copy);
    }
    for edge in layer.edges().unwrap() {
        let id = layer.edge_id(edge).unwrap().map(Into::into);
        let label = layer.edge_label(edge).unwrap().map(Into::into);
        let (from, to) = layer.edge_endpoints(edge).unwrap().unwrap();
        let properties = layer.edge_properties(edge).unwrap().cloned();
        graph
            .insert_edge(
                id,
                label,
                vertices[&from],
                vertices[&to],
                properties.unwrap_or_default(),
            )
            .unwrap();
    }
    let definitions: Vec<Definition> = layer
        .base()
        .definitions()
        .unwrap()
        .filter(|definition| definition.as_property_index().is_some())
        .cloned()
        .collect();
    for definition in definitions {
        graph.set_definition(definition).unwrap();
    }
    graph
}

/// The probes of one index: an equality on every value its key holds in `graph` (array
/// elements and lowered strings too), on absent values, and ranges of every kind the fixture
/// uses (open, closed, half-open, and one over two kinds that holds nothing).
fn probes(graph: &TraversalGraph, info: &PropertyIndexInfo) -> Vec<PropertyIndexRange> {
    let mut values: Vec<ElementProperty> = Vec::new();
    let mut add = |value: &ElementProperty| {
        if let ElementProperty::String(text) = value {
            values.push(ElementProperty::from(text.to_lowercase().as_str()));
        }
        values.push(value.clone());
    };
    let maps: Vec<PropertyMap> = match info.element {
        ElementKind::Vertex => graph
            .vertices()
            .unwrap()
            .filter_map(|node| graph.vertex_properties(node).unwrap().cloned())
            .collect(),
        _ => graph
            .edges()
            .unwrap()
            .filter_map(|edge| graph.edge_properties(edge).unwrap().cloned())
            .collect(),
    };
    for map in &maps {
        match map.get(&info.key) {
            Some(ElementProperty::Array(items)) => items.iter().for_each(&mut add),
            Some(value) => add(value),
            None => {}
        }
    }
    let mut ranges: Vec<PropertyIndexRange> =
        values.into_iter().map(PropertyIndexRange::equal).collect();
    ranges.extend([
        PropertyIndexRange::equal("absent".into()),
        PropertyIndexRange::equal(12345.into()),
        PropertyIndexRange::new(Bound::Included(0.into()), Bound::Excluded(100.into())),
        PropertyIndexRange::new(Bound::Excluded(29.into()), Bound::Unbounded),
        PropertyIndexRange::new(Bound::Included(0.0.into()), Bound::Included(1.0.into())),
        PropertyIndexRange::new(Bound::Included("a".into()), Bound::Excluded("n".into())),
        PropertyIndexRange::new(Bound::Unbounded, Bound::Included("zz".into())),
        PropertyIndexRange::new(Bound::Included(1.into()), Bound::Included("a".into())),
    ]);
    ranges
}

/// The ids a lookup of `info` over `range` answers on `graph`, in its order; `None` when the
/// storage does not serve the index.
fn lookup<S: GraphStorage>(
    graph: &S,
    info: &PropertyIndexInfo,
    range: &PropertyIndexRange,
) -> Option<Vec<String>> {
    let label = info.label.as_deref();
    let key = info.key.as_str();
    match (info.element, info.transform.as_ref()) {
        (ElementKind::Vertex, None) => graph
            .vertices_by_property_index(label, key, range)
            .unwrap()
            .map(|found| ids_of_vertices(graph, found)),
        (ElementKind::Vertex, Some(transform)) => graph
            .vertices_by_transformed_property_index(label, key, transform, range)
            .unwrap()
            .map(|found| ids_of_vertices(graph, found)),
        (_, None) => graph
            .edges_by_property_index(label, key, range)
            .unwrap()
            .map(|found| ids_of_edges(graph, found)),
        (_, Some(transform)) => graph
            .edges_by_transformed_property_index(label, key, transform, range)
            .unwrap()
            .map(|found| ids_of_edges(graph, found)),
    }
}

fn ids_of_vertices<S: GraphStorage>(
    graph: &S,
    found: Box<dyn Iterator<Item = S::VertexId> + '_>,
) -> Vec<String> {
    found
        .map(|node| graph.vertex_id(node).unwrap().unwrap().to_string())
        .collect()
}

fn ids_of_edges<S: GraphStorage>(
    graph: &S,
    found: Box<dyn Iterator<Item = S::EdgeId> + '_>,
) -> Vec<String> {
    found
        .map(|edge| graph.edge_id(edge).unwrap().unwrap().to_string())
        .collect()
}

/// The overlays are exact (`check`), and every lookup, count and stat of the layer equals the
/// materialized graph's: lookups the same ids in the same order; counts equal (an `each` index
/// may answer `None` on one side where the other counts: a part of the layer, or the
/// materialized graph, holding more than one key of the slice; then the answered count must
/// equal the lookup's length); stats with the same entries and skipped values,
/// `distinct_values` an upper bound.
#[track_caller]
fn same_as_materialized(layer: &LayeredGraph) {
    check(layer);
    let plain = materialize(layer);
    plain.check_property_indexes().unwrap();
    // By name: the materialized graph defines its indexes in the catalog's order.
    let by_name = |mut infos: Vec<PropertyIndexInfo>| {
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        infos
    };
    let served = by_name(layer.property_indexes().unwrap());
    let expected = by_name(plain.property_indexes().unwrap());
    assert_eq!(served.len(), expected.len());
    for (got, want) in served.iter().zip(&expected) {
        assert_eq!(
            (&got.name, got.element, &got.label, &got.key, &got.transform),
            (
                &want.name,
                want.element,
                &want.label,
                &want.key,
                &want.transform
            )
        );
        assert_eq!(got.stats.entries, want.stats.entries, "{}", want.name);
        assert_eq!(got.stats.skipped, want.stats.skipped, "{}", want.name);
        assert!(
            got.stats.distinct_values >= want.stats.distinct_values,
            "{}: {:?} vs {:?}",
            want.name,
            got.stats,
            want.stats
        );
    }
    let mut probed = 0;
    for info in &expected {
        for range in probes(&plain, info) {
            probed += 1;
            let want = lookup(&plain, info, &range);
            let got = lookup(layer, info, &range);
            assert!(want.is_some(), "{}", info.name);
            assert_eq!(got, want, "{} over {range:?}", info.name);
            let got_count = layer
                .count_by_property_index(
                    info.element,
                    info.label.as_deref(),
                    &info.key,
                    info.transform.as_ref(),
                    &range,
                )
                .unwrap();
            let want_count = plain
                .count_by_property_index(
                    info.element,
                    info.label.as_deref(),
                    &info.key,
                    info.transform.as_ref(),
                    &range,
                )
                .unwrap();
            let each = info.transform.as_ref().is_some_and(|t| t.has_each());
            let found = want.as_ref().map_or(0, Vec::len) as u64;
            if got_count != want_count {
                assert!(
                    each && (got_count.is_none() || want_count.is_none()),
                    "{} over {range:?}: count {got_count:?}, materialized {want_count:?}",
                    info.name
                );
            }
            for answered in [got_count, want_count].into_iter().flatten() {
                assert_eq!(answered, found, "{} over {range:?}", info.name);
            }
        }
    }
    assert!(probed > 50, "{probed}");
}

#[test]
fn an_untouched_layer_reads_like_its_base() {
    let layer = layer();
    same_as_materialized(&layer);
    let base = layer.base();
    for (got, want) in layer
        .property_indexes()
        .unwrap()
        .iter()
        .zip(base.property_indexes().unwrap())
    {
        assert_eq!(got.stats.entries, want.stats.entries);
        assert_eq!(got.stats.distinct_values, want.stats.distinct_values);
        assert_eq!(got.stats.bytes, 0, "the base part is the base's bytes");
    }
}

/// Every write kind of the S4.1 scenarios, each followed by the comparison with the
/// materialized graph.
#[test]
fn lookups_counts_and_stats_follow_every_write_kind() {
    let mut layer = layer();
    let marko = v(&layer, "1");
    let lop = v(&layer, "3");
    // Added vertices, with an `each` array holding a value twice and a skipped object.
    let ada = layer
        .insert_vertex(
            None,
            vec!["person".into()],
            props(&[
                ("name", "Ada".into()),
                ("age", 29.into()),
                (
                    "tags",
                    ElementProperty::Array(vec!["a".into(), "a".into(), "z".into()]),
                ),
            ]),
        )
        .unwrap();
    same_as_materialized(&layer);
    let unlabeled = layer
        .insert_vertex(None, vec![], props(&[("name", "MARKO".into())]))
        .unwrap();
    same_as_materialized(&layer);
    // Added edges: added-added, cross, base-base, unlabeled self-loop.
    let k1 = layer
        .insert_edge(
            None,
            Some("knows".into()),
            ada,
            unlabeled,
            props(&[("weight", 0.5.into())]),
        )
        .unwrap();
    layer
        .insert_edge(
            None,
            Some("knows".into()),
            marko,
            ada,
            props(&[("weight", 1.0.into())]),
        )
        .unwrap();
    layer
        .insert_edge(
            None,
            Some("likes".into()),
            marko,
            v(&layer, "6"),
            props(&[("weight", 2.into())]),
        )
        .unwrap();
    layer
        .insert_edge(None, None, ada, ada, props(&[("weight", 0.4.into())]))
        .unwrap();
    same_as_materialized(&layer);
    // Property writes on base elements (copy-up), on added ones, jpath into `each`.
    layer
        .set_vertex_property(marko, "age".into(), 27.into())
        .unwrap();
    same_as_materialized(&layer);
    layer
        .set_vertex_property(marko, "name".into(), "Vadas".into())
        .unwrap();
    same_as_materialized(&layer);
    layer
        .set_vertex_property(ada, "age".into(), 32.into())
        .unwrap();
    layer
        .set_vertex_property_path(marko, &path("tags[2]"), "a".into())
        .unwrap();
    same_as_materialized(&layer);
    layer
        .set_vertex_property_path(v(&layer, "4"), &path("meta.city"), "Rome".into())
        .unwrap();
    layer.drop_vertex_property("name", v(&layer, "2")).unwrap();
    same_as_materialized(&layer);
    // Labels: a base vertex gains and loses labels, an added one too; edge relabels.
    layer.add_vertex_label(lop, "person".into()).unwrap();
    same_as_materialized(&layer);
    layer.remove_vertex_label(lop, "software").unwrap();
    layer.remove_vertex_label(marko, "person").unwrap();
    same_as_materialized(&layer);
    layer.add_vertex_label(unlabeled, "person".into()).unwrap();
    layer
        .set_edge_label(e(&layer, "1"), "knows".into())
        .unwrap();
    layer.set_edge_label(e(&layer, "0"), "met".into()).unwrap();
    layer.set_edge_label(k1, "met".into()).unwrap();
    same_as_materialized(&layer);
    layer
        .set_edge_property(e(&layer, "3"), "weight".into(), 0.5.into())
        .unwrap();
    same_as_materialized(&layer);
    // Back to clean for one element, then drops: base (cascade), added, a base edge.
    layer.add_vertex_label(marko, "person".into()).unwrap();
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    same_as_materialized(&layer);
    layer.drop_vertex(v(&layer, "4")).unwrap();
    same_as_materialized(&layer);
    layer.drop_vertex(ada).unwrap();
    layer.drop_edge(e(&layer, "5")).unwrap();
    same_as_materialized(&layer);
    // A dropped base id reused.
    layer
        .insert_vertex(
            Some("4".into()),
            vec!["person".into()],
            props(&[("name", "josh".into()), ("age", 32.into())]),
        )
        .unwrap();
    same_as_materialized(&layer);
}

/// A rolled-back unit (and a nested savepoint) leaves the lookups as before the unit.
#[test]
fn lookups_after_a_rollback_equal_the_materialized_graph() {
    let mut layer = layer();
    let marko = v(&layer, "1");
    layer
        .set_vertex_property(marko, "age".into(), 33.into())
        .unwrap();
    let ada = layer
        .insert_vertex(
            None,
            vec!["person".into()],
            props(&[("age", 33.into()), ("name", "Ada".into())]),
        )
        .unwrap();
    same_as_materialized(&layer);
    let outer = layer.begin_unit().unwrap();
    layer
        .set_vertex_property(v(&layer, "2"), "age".into(), 33.into())
        .unwrap();
    let inner = layer.begin_unit().unwrap();
    layer.drop_vertex(ada).unwrap();
    layer.drop_vertex(marko).unwrap();
    layer
        .insert_vertex(None, vec!["person".into()], props(&[("age", 33.into())]))
        .unwrap();
    same_as_materialized(&layer);
    layer
        .rollback_unit(inner, &RollbackReason::failed(&"inner"))
        .unwrap();
    same_as_materialized(&layer);
    layer
        .set_edge_property(e(&layer, "0"), "weight".into(), 0.4.into())
        .unwrap();
    layer
        .rollback_unit(outer, &RollbackReason::failed(&"outer"))
        .unwrap();
    same_as_materialized(&layer);
    assert_eq!(
        layer.vertex_properties(marko).unwrap().unwrap().get("age"),
        Some(&33.into())
    );
}

/// On a touched layer the plan still reads the index (`.profile()` names it, exact, counting
/// from the postings), the storage answers it (no scan fallback), and the results equal the
/// materialized graph's, with the re-check dropped.
#[cfg(feature = "script")]
#[test]
fn a_touched_layer_keeps_its_index_pushdowns() {
    use graphersal::storage::Graph;
    let graph = Arc::new(Graph::new(layer()));
    let run = |graph: &Arc<Graph<LayeredGraph>>, script: &str| {
        crate::script_host::eval_value_with_limits(
            Arc::clone(graph),
            script,
            HashMap::new(),
            &crate::script_host::ScriptLimits::default(),
        )
        .unwrap()
    };
    for script in [
        r#"g.v("1").property("name", "MARKO").property("age", 30).iterate()"#,
        r#"g.add_v("person").property("name", "marko").property("age", 30).iterate()"#,
        r#"g.v("2").drop().iterate()"#,
        r#"g.e("0").property("weight", 0.9).iterate()"#,
    ] {
        let _unit = run(&graph, script);
    }
    {
        let layer = graph.read();
        check(&layer);
        let range = PropertyIndexRange::equal("marko".into());
        assert!(
            layer
                .vertices_by_transformed_property_index(
                    Some("person"),
                    "name",
                    &PropertyIndexTransform::ToLower,
                    &range,
                )
                .unwrap()
                .is_some(),
            "the storage answers a touched kind"
        );
        assert!(
            layer
                .count_by_property_index(ElementKind::Vertex, None, "name", None, &range)
                .unwrap()
                .is_some()
        );
    }
    let profile = |query: &str| -> String {
        let metrics = run(&graph, &format!("g.{query}.profile()"))
            .try_cast::<graphersal::profile::TraversalMetrics>()
            .expect("metrics");
        metrics.metrics()[0].name().to_string()
    };
    assert_eq!(
        profile(r#"v().has("name", "marko").count()"#),
        r#"v().has("name", "marko").count() [index: vertex name, any label, exact] [count from index]"#
    );
    let lowered =
        profile(r#"v().has_label("person").has("name", __.to_lower().is(P.eq("marko")))"#);
    assert!(
        lowered.contains("[index: vertex person.name to_lower"),
        "{lowered}"
    );
    let ranged = profile(r#"v().has_label("person").has("age", P.gte(30))"#);
    assert!(ranged.contains("[index: vertex person.age"), "{ranged}");
    let edge = profile(r#"e().has("weight", 0.9)"#);
    assert!(edge.contains("[index: edge weight"), "{edge}");

    let plain = Arc::new(Graph::new(materialize(&graph.read())));
    for query in [
        r#"g.v().has("name", "marko").id().to_list()"#,
        r#"g.v().has("name", "marko").count()"#,
        r#"g.v().has_label("person").has("name", __.to_lower().is(P.eq("marko"))).id().to_list()"#,
        r#"g.v().has_label("person").has("age", P.gte(30)).id().to_list()"#,
        r#"g.v().has_label("person").has("tags", __.unfold().is(P.eq("a"))).id().to_list()"#,
        r#"g.v().has_label("person").has("tags", __.unfold().is(P.eq("a"))).count()"#,
        r#"g.e().has("weight", 0.9).id().to_list()"#,
        r#"g.e().has_label("knows").has("weight", P.lt(1.0)).id().to_list()"#,
    ] {
        let got = run(&graph, query).to_string();
        let crate_plain = crate::script_host::eval_value_with_limits(
            Arc::clone(&plain),
            query,
            HashMap::new(),
            &crate::script_host::ScriptLimits::default(),
        )
        .unwrap()
        .to_string();
        assert_eq!(got, crate_plain, "{query}");
    }
}
