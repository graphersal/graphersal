//! Delta visibility of a layered storage (S2.1): `GraphStorage::vertex_layer` / `edge_layer`
//! (an element's `LayerOrigin`) and `GraphStorage::layer_diff` (what a layer changed against its
//! base). A plain `TraversalGraph` answers `Base` and an empty diff; a `LayeredGraph` answers
//! from its delta: added, modified (properties, labels), changed back (`Base` again), removed
//! (with the base edges a removed vertex took along, reported with their base values), cross
//! edges between base vertices, the kind, label and limit filters with `truncated`, the order,
//! and the dyn boundary (`AnyGraph::layer_diff`).

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{
    ElementKind, Graph, GraphStorage, IntoAnyGraph, LayerDiff, LayerDiffRequest, LayerOrigin,
    LayerPropertyChange, LayeredGraph,
};

fn modern_layer() -> LayeredGraph {
    LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern())).unwrap()
}

fn vertex(layer: &LayeredGraph, id: &str) -> graphersal::storage::LayeredVertex {
    layer.get_vertex_by_id(id).unwrap().unwrap()
}

fn edge(layer: &LayeredGraph, id: &str) -> graphersal::storage::LayeredEdge {
    layer.get_edge_by_id(id).unwrap().unwrap()
}

fn diff(layer: &LayeredGraph) -> LayerDiff {
    layer.layer_diff(&LayerDiffRequest::new()).unwrap()
}

fn ids<'a>(entries: impl IntoIterator<Item = &'a CompactString>) -> Vec<&'a str> {
    entries.into_iter().map(CompactString::as_str).collect()
}

fn added_ids(diff: &LayerDiff) -> Vec<&str> {
    ids(diff.added.iter().map(|e| &e.id))
}

fn removed_ids(diff: &LayerDiff) -> Vec<&str> {
    ids(diff.removed.iter().map(|e| &e.id))
}

fn modified_ids(diff: &LayerDiff) -> Vec<&str> {
    ids(diff.modified.iter().map(|e| &e.id))
}

fn origins(layer: &LayeredGraph) -> (Vec<(String, LayerOrigin)>, Vec<(String, LayerOrigin)>) {
    let vertices = layer
        .vertices()
        .unwrap()
        .map(|v| {
            let id = layer.vertex_id(v).unwrap().unwrap().to_string();
            (id, layer.vertex_layer(v).unwrap().unwrap())
        })
        .collect();
    let edges = layer
        .edges()
        .unwrap()
        .map(|e| {
            let id = layer.edge_id(e).unwrap().unwrap().to_string();
            (id, layer.edge_layer(e).unwrap().unwrap())
        })
        .collect();
    (vertices, edges)
}

fn all_base(layer: &LayeredGraph) -> bool {
    let (vertices, edges) = origins(layer);
    vertices
        .iter()
        .chain(&edges)
        .all(|(_, o)| *o == LayerOrigin::Base)
}

fn origin_of(list: &[(String, LayerOrigin)], id: &str) -> LayerOrigin {
    list.iter().find(|(i, _)| i == id).map(|(_, o)| *o).unwrap()
}

fn props(pairs: &[(&str, ElementProperty)]) -> PropertyMap {
    pairs.iter().map(|(k, v)| (*k, v.clone())).collect()
}

#[test]
fn a_plain_graph_answers_base_and_an_empty_diff() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let vertices: Vec<_> = graph.vertices().unwrap().collect();
    let edges: Vec<_> = graph.edges().unwrap().collect();
    assert!(
        vertices
            .iter()
            .all(|&v| graph.vertex_layer(v).unwrap() == Some(LayerOrigin::Base))
    );
    assert!(
        edges
            .iter()
            .all(|&e| graph.edge_layer(e).unwrap() == Some(LayerOrigin::Base))
    );
    let empty = graph.layer_diff(&LayerDiffRequest::new()).unwrap();
    assert!(empty.is_empty() && !empty.truncated);
    assert_eq!(empty, LayerDiff::default());
    // A removed element has no origin, as the other element reads answer for it.
    graph.drop_vertex(vertices[0]).unwrap();
    assert_eq!(graph.vertex_layer(vertices[0]).unwrap(), None);
    assert_eq!(graph.edge_layer(edges[0]).unwrap(), None);
    // The shared view and an `Arc` forward to the same answers.
    assert_eq!(
        GraphStorage::vertex_layer(&&graph, vertices[1]).unwrap(),
        Some(LayerOrigin::Base)
    );
    let shared = Arc::new(graph);
    assert_eq!(
        GraphStorage::vertex_layer(&shared, vertices[1]).unwrap(),
        Some(LayerOrigin::Base)
    );
    assert!(
        GraphStorage::layer_diff(&shared, &LayerDiffRequest::new())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_untouched_layer_is_all_base() {
    let layer = modern_layer();
    assert!(all_base(&layer));
    assert!(diff(&layer).is_empty());
    assert_eq!(LayerOrigin::Base.as_str(), "base");
    assert_eq!(LayerOrigin::Added.as_str(), "added");
    assert_eq!(LayerOrigin::Modified.as_str(), "modified");
}

#[test]
fn added_elements_are_added_with_their_layer_values() {
    let mut layer = modern_layer();
    let ada = layer
        .insert_vertex(
            Some("ada".into()),
            vec!["person".into(), "admin".into()],
            props(&[("name", "ada".into())]),
        )
        .unwrap();
    let lop = vertex(&layer, "3");
    let made = layer
        .insert_edge(
            Some("e1".into()),
            Some("created".into()),
            ada,
            lop,
            props(&[("weight", 0.7.into())]),
        )
        .unwrap();
    assert_eq!(layer.vertex_layer(ada).unwrap(), Some(LayerOrigin::Added));
    assert_eq!(layer.edge_layer(made).unwrap(), Some(LayerOrigin::Added));
    // lop only gained an incident edge: it stays Base.
    assert_eq!(layer.vertex_layer(lop).unwrap(), Some(LayerOrigin::Base));
    let d = diff(&layer);
    assert!(d.removed.is_empty() && d.modified.is_empty() && !d.truncated);
    assert_eq!(added_ids(&d), ["ada", "e1"]);
    let v = &d.added[0];
    assert_eq!(v.kind, ElementKind::Vertex);
    assert_eq!(ids(&v.labels), ["person", "admin"]);
    assert_eq!(v.endpoints, None);
    assert_eq!(v.properties, props(&[("name", "ada".into())]));
    let e = &d.added[1];
    assert_eq!(e.kind, ElementKind::Edge);
    assert_eq!(ids(&e.labels), ["created"]);
    assert_eq!(e.endpoints, Some(("ada".into(), "3".into())));
    assert_eq!(e.properties, props(&[("weight", 0.7.into())]));
    // Dropping an added element takes it out of the diff.
    layer.drop_vertex(ada).unwrap();
    assert!(diff(&layer).is_empty());
    assert!(all_base(&layer));
}

#[test]
fn modified_properties_and_the_return_to_base() {
    let mut layer = modern_layer();
    let marko = vertex(&layer, "1");
    // A write of the base's value changes nothing.
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    assert_eq!(layer.vertex_layer(marko).unwrap(), Some(LayerOrigin::Base));
    assert!(diff(&layer).is_empty());

    layer
        .set_vertex_property(marko, "age".into(), 30.into())
        .unwrap();
    layer
        .set_vertex_property(marko, "nick".into(), "mk".into())
        .unwrap();
    layer.drop_vertex_property("name", marko).unwrap();
    assert_eq!(
        layer.vertex_layer(marko).unwrap(),
        Some(LayerOrigin::Modified)
    );
    let d = diff(&layer);
    assert!(d.added.is_empty() && d.removed.is_empty());
    assert_eq!(modified_ids(&d), ["1"]);
    let change = &d.modified[0];
    assert_eq!(change.kind, ElementKind::Vertex);
    assert_eq!(ids(&change.base_labels), ["person"]);
    assert_eq!(ids(&change.labels), ["person"]);
    assert_eq!(change.endpoints, None);
    assert_eq!(
        change.properties,
        [
            LayerPropertyChange::new("age", Some(29.into()), Some(30.into())),
            LayerPropertyChange::new("nick", None, Some("mk".into())),
            LayerPropertyChange::new("name", Some("marko".into()), None),
        ]
    );

    // Changed back (in the base's order): Base again, out of the diff.
    layer.drop_vertex_property("nick", marko).unwrap();
    layer.drop_vertex_property("age", marko).unwrap();
    layer
        .set_vertex_property(marko, "name".into(), "marko".into())
        .unwrap();
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    assert_eq!(layer.vertex_layer(marko).unwrap(), Some(LayerOrigin::Base));
    assert!(diff(&layer).is_empty());
    assert!(all_base(&layer));
}

#[test]
fn modified_labels_of_vertices_and_edges() {
    let mut layer = modern_layer();
    let josh = vertex(&layer, "4");
    layer.add_vertex_label(josh, "admin".into()).unwrap();
    let knows = edge(&layer, "0");
    layer
        .set_edge_property(knows, "weight".into(), 0.9.into())
        .unwrap();
    let created = edge(&layer, "1");
    layer.set_edge_label(created, "made".into()).unwrap();
    let (vertices, edges) = origins(&layer);
    assert_eq!(origin_of(&vertices, "4"), LayerOrigin::Modified);
    assert_eq!(origin_of(&vertices, "1"), LayerOrigin::Base);
    assert_eq!(origin_of(&edges, "0"), LayerOrigin::Modified);
    assert_eq!(origin_of(&edges, "1"), LayerOrigin::Modified);
    assert_eq!(origin_of(&edges, "2"), LayerOrigin::Base);

    let d = diff(&layer);
    assert_eq!(
        modified_ids(&d),
        ["4", "0", "1"],
        "vertices first, then base slot order"
    );
    let josh_change = &d.modified[0];
    assert_eq!(ids(&josh_change.base_labels), ["person"]);
    assert_eq!(ids(&josh_change.labels), ["person", "admin"]);
    assert!(josh_change.properties.is_empty());
    let knows_change = &d.modified[1];
    assert_eq!(knows_change.kind, ElementKind::Edge);
    assert_eq!(knows_change.endpoints, Some(("1".into(), "2".into())));
    assert_eq!(ids(&knows_change.base_labels), ["knows"]);
    assert_eq!(ids(&knows_change.labels), ["knows"]);
    assert_eq!(
        knows_change.properties,
        [LayerPropertyChange::new(
            "weight",
            Some(0.5.into()),
            Some(0.9.into())
        )]
    );
    let created_change = &d.modified[2];
    assert_eq!(ids(&created_change.base_labels), ["created"]);
    assert_eq!(ids(&created_change.labels), ["made"]);
    assert_eq!(created_change.endpoints, Some(("1".into(), "3".into())));
    assert!(created_change.properties.is_empty());

    // Relabeled back: Base again.
    layer.set_edge_label(created, "created".into()).unwrap();
    layer.remove_vertex_label(josh, "admin").unwrap();
    assert_eq!(layer.edge_layer(created).unwrap(), Some(LayerOrigin::Base));
    assert_eq!(layer.vertex_layer(josh).unwrap(), Some(LayerOrigin::Base));
    assert_eq!(modified_ids(&diff(&layer)), ["0"]);
}

#[test]
fn a_removed_vertex_reports_its_base_values_and_its_base_edges() {
    let mut layer = modern_layer();
    let marko = vertex(&layer, "1");
    let vadas = vertex(&layer, "2");
    let knows = edge(&layer, "0");
    // Modified first: the diff still reports the BASE values of a removed element.
    layer
        .set_vertex_property(marko, "age".into(), 99.into())
        .unwrap();
    layer
        .set_edge_property(knows, "weight".into(), 0.1.into())
        .unwrap();
    layer.drop_vertex(marko).unwrap();
    assert_eq!(layer.vertex_layer(marko).unwrap(), None);
    assert_eq!(layer.edge_layer(knows).unwrap(), None);
    // vadas lost an incident edge only: Base.
    assert_eq!(layer.vertex_layer(vadas).unwrap(), Some(LayerOrigin::Base));
    assert!(all_base(&layer));

    let d = diff(&layer);
    assert!(d.added.is_empty() && d.modified.is_empty() && !d.truncated);
    assert_eq!(removed_ids(&d), ["1", "0", "1", "2"]);
    let v = &d.removed[0];
    assert_eq!(v.kind, ElementKind::Vertex);
    assert_eq!(ids(&v.labels), ["person"]);
    assert_eq!(
        v.properties,
        props(&[("name", "marko".into()), ("age", 29.into())])
    );
    let expected = [
        ("0", "knows", "2", 0.5),
        ("1", "created", "3", 0.4),
        ("2", "knows", "4", 1.0),
    ];
    for (entry, (id, label, to, weight)) in d.removed[1..].iter().zip(expected) {
        assert_eq!(entry.kind, ElementKind::Edge);
        assert_eq!(entry.id, id);
        assert_eq!(ids(&entry.labels), [label]);
        assert_eq!(entry.endpoints, Some(("1".into(), to.into())));
        assert_eq!(entry.properties, props(&[("weight", weight.into())]));
    }
}

#[test]
fn a_cross_edge_between_base_vertices_is_added_and_its_ends_stay_base() {
    let mut layer = modern_layer();
    let peter = vertex(&layer, "6");
    let vadas = vertex(&layer, "2");
    let met = layer
        .insert_edge(
            Some("met".into()),
            Some("knows".into()),
            peter,
            vadas,
            PropertyMap::default(),
        )
        .unwrap();
    assert_eq!(layer.edge_layer(met).unwrap(), Some(LayerOrigin::Added));
    assert_eq!(layer.vertex_layer(peter).unwrap(), Some(LayerOrigin::Base));
    assert_eq!(layer.vertex_layer(vadas).unwrap(), Some(LayerOrigin::Base));
    let d = diff(&layer);
    assert_eq!(added_ids(&d), ["met"]);
    assert_eq!(d.added[0].endpoints, Some(("6".into(), "2".into())));
    assert!(d.added[0].properties.is_empty());
    assert!(d.removed.is_empty() && d.modified.is_empty());
}

/// A layer with entries of every kind: added vertex `n1` (person) and `n2` (software), added
/// edge `e1` (knows, n1 -> 2), removed vertex 5 (software) with its base edge 4 (created),
/// modified vertex 2 (person) and modified edge 5 (created).
fn busy_layer() -> LayeredGraph {
    let mut layer = modern_layer();
    let n1 = layer
        .insert_vertex(
            Some("n1".into()),
            vec!["person".into()],
            PropertyMap::default(),
        )
        .unwrap();
    layer
        .insert_vertex(
            Some("n2".into()),
            vec!["software".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let vadas = vertex(&layer, "2");
    layer
        .insert_edge(
            Some("e1".into()),
            Some("knows".into()),
            n1,
            vadas,
            PropertyMap::default(),
        )
        .unwrap();
    let ripple = vertex(&layer, "5");
    layer.drop_vertex(ripple).unwrap();
    layer
        .set_vertex_property(vadas, "age".into(), 28.into())
        .unwrap();
    let e5 = edge(&layer, "5");
    layer
        .set_edge_property(e5, "weight".into(), 0.3.into())
        .unwrap();
    layer
}

#[test]
fn the_diff_order_is_deterministic() {
    let layer = busy_layer();
    let d = diff(&layer);
    assert_eq!(added_ids(&d), ["n1", "n2", "e1"]);
    assert_eq!(removed_ids(&d), ["5", "4"]);
    assert_eq!(modified_ids(&d), ["2", "5"]);
    assert_eq!(d.len(), 7);
    assert!(!d.truncated);
    assert_eq!(diff(&layer), d, "the same answer twice");

    // Removed elements list in base slot order, whatever order they were dropped in.
    let mut layer = modern_layer();
    let lop = vertex(&layer, "3");
    let vadas = vertex(&layer, "2");
    let e4 = edge(&layer, "4");
    let e0 = edge(&layer, "0");
    layer.drop_edge(e4).unwrap();
    layer.drop_vertex(lop).unwrap();
    layer.drop_edge(e0).unwrap();
    layer.drop_vertex(vadas).unwrap();
    let d = diff(&layer);
    assert_eq!(removed_ids(&d), ["2", "3", "0", "1", "3", "4", "5"]);
}

#[test]
fn the_kind_label_and_limit_filters() {
    let layer = busy_layer();
    let only = |request: LayerDiffRequest| layer.layer_diff(&request).unwrap();

    let vertices = only(LayerDiffRequest::new().kind(ElementKind::Vertex));
    assert_eq!(added_ids(&vertices), ["n1", "n2"]);
    assert_eq!(removed_ids(&vertices), ["5"]);
    assert_eq!(modified_ids(&vertices), ["2"]);
    let edges = only(LayerDiffRequest::new().kind(ElementKind::Edge));
    assert_eq!(added_ids(&edges), ["e1"]);
    assert_eq!(removed_ids(&edges), ["4"]);
    assert_eq!(modified_ids(&edges), ["5"]);

    let software = only(LayerDiffRequest::new().labels(["software"]));
    assert_eq!(added_ids(&software), ["n2"]);
    assert_eq!(removed_ids(&software), ["5"]);
    assert!(software.modified.is_empty());
    let created = only(LayerDiffRequest::new().labels(["created", "nothing"]));
    assert!(created.added.is_empty());
    assert_eq!(removed_ids(&created), ["4"]);
    assert_eq!(modified_ids(&created), ["5"]);
    let none = only(LayerDiffRequest::new().labels(Vec::<&str>::new()));
    assert!(none.is_empty() && !none.truncated);

    // The limit counts entries across the whole diff: vertices (added, removed, modified),
    // then edges.
    let three = only(LayerDiffRequest::new().limit(3));
    assert_eq!(added_ids(&three), ["n1", "n2"]);
    assert_eq!(removed_ids(&three), ["5"]);
    assert!(three.modified.is_empty());
    assert!(three.truncated);
    let five = only(LayerDiffRequest::new().limit(5));
    assert_eq!(added_ids(&five), ["n1", "n2", "e1"]);
    assert_eq!(modified_ids(&five), ["2"]);
    assert!(five.truncated);
    let exact = only(LayerDiffRequest::new().limit(7));
    assert_eq!(exact.len(), 7);
    assert!(!exact.truncated, "nothing left out");
    let zero = only(LayerDiffRequest::new().limit(0));
    assert!(zero.is_empty() && zero.truncated);
    let filtered = only(LayerDiffRequest::new().labels(["person"]).limit(2));
    assert_eq!(added_ids(&filtered), ["n1"]);
    assert_eq!(modified_ids(&filtered), ["2"]);
    assert!(
        !filtered.truncated,
        "the limit counts matching entries only"
    );

    // A modified element matches with its base labels or with its layer labels.
    let mut layer = modern_layer();
    let lop = vertex(&layer, "3");
    layer.add_vertex_label(lop, "tool".into()).unwrap();
    layer.remove_vertex_label(lop, "software").unwrap();
    for label in ["software", "tool"] {
        let d = layer
            .layer_diff(&LayerDiffRequest::new().labels([label]))
            .unwrap();
        assert_eq!(modified_ids(&d), ["3"], "{label}");
    }
}

#[test]
fn the_diff_reaches_the_dyn_boundary() {
    let graph = Arc::new(Graph::new(busy_layer()));
    let any = Arc::clone(&graph).into_any_graph();
    let d = any.layer_diff(&LayerDiffRequest::new()).unwrap();
    assert_eq!(
        d,
        graph.read().layer_diff(&LayerDiffRequest::new()).unwrap()
    );
    assert_eq!(d.len(), 7);
    let plain = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern())).into_any_graph();
    assert!(
        plain
            .layer_diff(&LayerDiffRequest::new())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_rolled_back_unit_leaves_no_trace_in_the_diff() {
    use graphersal::storage::RollbackReason;
    let mut layer = modern_layer();
    let marko = vertex(&layer, "1");
    let mark = layer.begin_unit().unwrap();
    layer
        .set_vertex_property(marko, "age".into(), 1.into())
        .unwrap();
    layer
        .insert_vertex(None, vec!["x".into()], PropertyMap::default())
        .unwrap();
    layer.drop_vertex(vertex(&layer, "6")).unwrap();
    assert_eq!(
        diff(&layer).len(),
        4,
        "1 modified, 1 added, vertex 6 and its edge 5"
    );
    layer
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert!(diff(&layer).is_empty());
    assert!(all_base(&layer));
}
