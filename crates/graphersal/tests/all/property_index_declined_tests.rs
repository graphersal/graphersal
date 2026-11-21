//! `.profile()` tells the truth when a storage lists a property index but declines it at run
//! time (S4.8): the plan reads `[index: ..]`, the lookup answers `None`, the source scans, and
//! the profile shows `declined, scanned` inside the brackets; a fused `count()` the storage
//! does not answer from the postings shows `[count from index declined]`. A storage that serves
//! its indexes (`TraversalGraph`, a touched `LayeredGraph`) never shows "declined".

use std::sync::Arc;

use graphersal::catalog::{Definition, PropertyIndexDefinition};
use graphersal::prelude::*;
use graphersal::storage::{
    CompactString, EdgeHandle, ElementKind, GraphError, GraphStorage, LayeredGraph,
    PropertyIndexInfo, PropertyIndexRange, PropertyMap, StorageCapabilities, VertexHandle,
};

/// The modern graph with indexes on `person.name`, `person.age` and a label-less one on the
/// edges' `weight`.
fn indexed() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for (name, definition) in [
        (
            "name",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
        ),
        (
            "age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        ),
        (
            "weight",
            PropertyIndexDefinition::label_less(ElementKind::Edge, "weight"),
        ),
    ] {
        graph
            .set_definition(Definition::property_index(name, definition))
            .unwrap();
    }
    graph
}

/// A storage that lists the property indexes of the graph it wraps (so the planner picks
/// them, exact where the shape allows) but declines them at run time: every lookup answers
/// `None` unless `lookups`, and `count_by_property_index` always keeps the trait default
/// `None`.
struct Declining {
    graph: TraversalGraph,
    lookups: bool,
}

impl GraphStorage for Declining {
    type VertexId = VertexHandle;
    type EdgeId = EdgeHandle;

    fn vertex_count(&self) -> Result<usize, GraphError> {
        GraphStorage::vertex_count(&self.graph)
    }
    fn edge_count(&self) -> Result<usize, GraphError> {
        GraphStorage::edge_count(&self.graph)
    }
    fn vertices(&self) -> Result<impl Iterator<Item = VertexHandle> + '_, GraphError> {
        GraphStorage::vertices(&self.graph)
    }
    fn edges(&self) -> Result<impl Iterator<Item = EdgeHandle> + '_, GraphError> {
        GraphStorage::edges(&self.graph)
    }
    fn get_vertex_by_id(&self, id: &str) -> Result<Option<VertexHandle>, GraphError> {
        GraphStorage::get_vertex_by_id(&self.graph, id)
    }
    fn get_edge_by_id(&self, id: &str) -> Result<Option<EdgeHandle>, GraphError> {
        GraphStorage::get_edge_by_id(&self.graph, id)
    }
    fn out_edges(
        &self,
        node: VertexHandle,
    ) -> Result<impl Iterator<Item = EdgeHandle> + '_, GraphError> {
        GraphStorage::out_edges(&self.graph, node)
    }
    fn in_edges(
        &self,
        node: VertexHandle,
    ) -> Result<impl Iterator<Item = EdgeHandle> + '_, GraphError> {
        GraphStorage::in_edges(&self.graph, node)
    }
    fn vertex_id(&self, node: VertexHandle) -> Result<Option<&str>, GraphError> {
        GraphStorage::vertex_id(&self.graph, node)
    }
    fn edge_id(&self, edge: EdgeHandle) -> Result<Option<&str>, GraphError> {
        GraphStorage::edge_id(&self.graph, edge)
    }
    fn vertex_label(&self, node: VertexHandle) -> Result<Option<&str>, GraphError> {
        GraphStorage::vertex_label(&self.graph, node)
    }
    fn edge_label(&self, edge: EdgeHandle) -> Result<Option<&str>, GraphError> {
        GraphStorage::edge_label(&self.graph, edge)
    }
    fn vertex_labels(&self, node: VertexHandle) -> Result<&[CompactString], GraphError> {
        GraphStorage::vertex_labels(&self.graph, node)
    }
    fn vertex_properties(&self, node: VertexHandle) -> Result<Option<&PropertyMap>, GraphError> {
        GraphStorage::vertex_properties(&self.graph, node)
    }
    fn edge_properties(&self, edge: EdgeHandle) -> Result<Option<&PropertyMap>, GraphError> {
        GraphStorage::edge_properties(&self.graph, edge)
    }
    fn edge_endpoints(
        &self,
        edge: EdgeHandle,
    ) -> Result<Option<(VertexHandle, VertexHandle)>, GraphError> {
        GraphStorage::edge_endpoints(&self.graph, edge)
    }
    fn insert_vertex(
        &mut self,
        id: Option<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Result<VertexHandle, GraphError> {
        GraphStorage::insert_vertex(&mut self.graph, id, labels, properties)
    }
    fn add_vertex_label(
        &mut self,
        node: VertexHandle,
        label: CompactString,
    ) -> Result<bool, GraphError> {
        GraphStorage::add_vertex_label(&mut self.graph, node, label)
    }
    fn remove_vertex_label(&mut self, node: VertexHandle, label: &str) -> Result<bool, GraphError> {
        GraphStorage::remove_vertex_label(&mut self.graph, node, label)
    }
    fn insert_edge(
        &mut self,
        id: Option<CompactString>,
        label: Option<CompactString>,
        from: VertexHandle,
        to: VertexHandle,
        properties: PropertyMap,
    ) -> Result<EdgeHandle, GraphError> {
        GraphStorage::insert_edge(&mut self.graph, id, label, from, to, properties)
    }
    fn set_vertex_property(
        &mut self,
        node: VertexHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_vertex_property(&mut self.graph, node, key, value)
    }
    fn set_edge_property(
        &mut self,
        edge: EdgeHandle,
        key: CompactString,
        value: ElementProperty,
    ) -> Result<(), GraphError> {
        GraphStorage::set_edge_property(&mut self.graph, edge, key, value)
    }
    fn drop_vertex(&mut self, node: VertexHandle) -> Result<(), GraphError> {
        GraphStorage::drop_vertex(&mut self.graph, node)
    }
    fn drop_vertex_property(&mut self, prop: &str, node: VertexHandle) -> Result<(), GraphError> {
        GraphStorage::drop_vertex_property(&mut self.graph, prop, node)
    }
    fn drop_edge(&mut self, edge: EdgeHandle) -> Result<(), GraphError> {
        GraphStorage::drop_edge(&mut self.graph, edge)
    }
    fn drop_edge_property(&mut self, prop: &str, edge: EdgeHandle) -> Result<(), GraphError> {
        GraphStorage::drop_edge_property(&mut self.graph, prop, edge)
    }
    fn capabilities(&self) -> Result<StorageCapabilities, GraphError> {
        Ok(StorageCapabilities::none()
            .with_writable(true)
            .with_property_indexes(true)
            .with_exact_property_index_lookups(true))
    }
    fn property_indexes(&self) -> Result<Vec<PropertyIndexInfo>, GraphError> {
        GraphStorage::property_indexes(&self.graph)
    }
    fn vertices_by_property_index<'a>(
        &'a self,
        label: Option<&'a str>,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = VertexHandle> + 'a>>, GraphError> {
        if !self.lookups {
            return Ok(None);
        }
        GraphStorage::vertices_by_property_index(&self.graph, label, key, range)
    }
    fn edges_by_property_index<'a>(
        &'a self,
        label: Option<&'a str>,
        key: &'a str,
        range: &PropertyIndexRange,
    ) -> Result<Option<Box<dyn Iterator<Item = EdgeHandle> + 'a>>, GraphError> {
        if !self.lookups {
            return Ok(None);
        }
        GraphStorage::edges_by_property_index(&self.graph, label, key, range)
    }
}

/// The full name of every step of a profile, nested ones after their holder (the table cuts
/// long names).
fn names(metrics: graphersal::profile::TraversalMetrics) -> Vec<String> {
    fn walk(metrics: &[graphersal::profile::Metrics], out: &mut Vec<String>) {
        for step in metrics {
            out.push(step.name().to_string());
            walk(step.nested(), out);
        }
    }
    let mut out = Vec::new();
    walk(metrics.metrics(), &mut out);
    out
}

/// The source names of the four probe queries on `storage`, and their results.
fn probe<S: GraphStorage>(storage: &S) -> (Vec<String>, Vec<String>) {
    let g = || GraphTraversalSource::new(storage);
    let mut profiles = Vec::new();
    let mut results = Vec::new();
    let by_name = || g().v(None).has_label("person").has("name", "marko");
    profiles.push(names(by_name().profile().unwrap())[0].clone());
    results.push(format!("{:?}", by_name().id().to_list().unwrap()));
    let by_age = || g().v(None).has_label("person").has_p("age", P::Gte(30));
    profiles.push(names(by_age().profile().unwrap())[0].clone());
    let mut ages = by_age().id().to_list().unwrap();
    ages.sort_by_key(|id| format!("{id:?}"));
    results.push(format!("{ages:?}"));
    let counted = || g().v(None).has_label("person").has("name", "marko").count();
    profiles.push(names(counted().profile().unwrap())[0].clone());
    results.push(format!("{:?}", counted().to_list().unwrap()));
    let edges = || g().e(None).has("weight", 0.5);
    profiles.push(names(edges().profile().unwrap())[0].clone());
    results.push(format!("{:?}", edges().id().to_list().unwrap()));
    (profiles, results)
}

#[test]
fn a_declined_lookup_shows_that_the_source_scanned() {
    let reference = indexed();
    let (served, expected) = probe(&reference);
    let declining = Declining {
        graph: indexed(),
        lookups: false,
    };
    let (profiles, results) = probe(&declining);
    assert_eq!(
        results, expected,
        "a declined index scans to the same results"
    );
    for (profile, served) in profiles.iter().zip(&served) {
        assert!(served.contains("[index: "), "{served}");
        assert!(!served.contains("declined"), "{served}");
        assert!(profile.contains(", declined, scanned]"), "{profile}");
    }
    assert_eq!(
        profiles[0],
        r#"v(labels: ["person"]).has("name", "marko") [index: vertex person.name, exact, declined, scanned]"#
    );
    assert_eq!(
        profiles[1],
        r#"v(labels: ["person"]) [index: vertex person.age, declined, scanned]"#
    );
    // Neither the lookup nor the postings count answered: both facts show.
    assert_eq!(
        profiles[2],
        r#"v(labels: ["person"]).has("name", "marko").count() [index: vertex person.name, exact, declined, scanned] [count from index declined]"#
    );
    assert_eq!(
        profiles[3],
        r#"e().has("weight", 0.5) [index: edge weight, any label, declined, scanned]"#
    );
}

#[test]
fn a_declined_count_shows_that_the_candidates_were_counted() {
    // The lookups are served, the postings count is not: only the count is declined.
    let declining = Declining {
        graph: indexed(),
        lookups: true,
    };
    let (profiles, results) = probe(&declining);
    let (_, expected) = probe(&indexed());
    assert_eq!(results, expected);
    assert!(
        profiles[2].ends_with("[count from index declined]"),
        "{}",
        profiles[2]
    );
    assert!(!profiles[2].contains("scanned"), "{}", profiles[2]);
    for profile in [&profiles[0], &profiles[1], &profiles[3]] {
        assert!(profile.contains("[index: "), "{profile}");
        assert!(!profile.contains("declined"), "{profile}");
    }
}

#[test]
fn a_mid_traversal_source_is_renamed_once() {
    // The child traversal runs once per software vertex (twice): the name of its source
    // records the fact once.
    let declining = Declining {
        graph: indexed(),
        lookups: false,
    };
    let g = GraphTraversalSource::new(&declining);
    let metrics = g
        .v(None)
        .has_label("software")
        .map(__::v(None).has_label("person").has("name", "marko").id())
        .profile()
        .unwrap();
    let names = names(metrics);
    let source = names
        .iter()
        .find(|name| name.starts_with("v(") && name.contains("[index: "))
        .unwrap_or_else(|| panic!("{names:?}"));
    assert_eq!(source.matches("declined").count(), 1, "{source}");
    assert!(source.ends_with(", exact, declined, scanned]"), "{source}");
}

#[test]
fn served_indexes_never_show_declined() {
    let (plain, _) = probe(&indexed());
    // A layer touched on every probed kind serves the overlays it lists.
    let mut layer = LayeredGraph::over(Arc::new(indexed())).unwrap();
    GraphTraversalSource::new(&mut layer)
        .v("1")
        .property("name", "marko")
        .property("age", 30)
        .iterate()
        .unwrap();
    GraphTraversalSource::new(&mut layer)
        .add_v(vec!["person"])
        .property("name", "marko")
        .property("age", 31)
        .iterate()
        .unwrap();
    GraphTraversalSource::new(&mut layer)
        .e("7")
        .property("weight", 0.5)
        .iterate()
        .unwrap();
    let (layered, _) = probe(&layer);
    for profile in plain.iter().chain(&layered) {
        assert!(profile.contains("[index: "), "{profile}");
        assert!(!profile.contains("declined"), "{profile}");
    }
    assert!(plain[2].ends_with("[count from index]"), "{}", plain[2]);
    assert!(layered[2].ends_with("[count from index]"), "{}", layered[2]);
}
