//! Streaming file imports through stub vertices (`GraphStorage::insert_vertex_stub` /
//! `complete_vertex_stub`): GraphSON and GraphML store every edge as soon as they read it; an
//! endpoint whose record comes later is a stub that its record completes. Checked here: the
//! stored data, label counts and indexes equal a plain build; change capture reports a completed
//! stub as ONE `AddVertex`; a rollback undoes stubs and completions; the schema's connection
//! check and unique indexes run when a stub is completed; the second copy of an edge is
//! recognized in an empty and in a non-empty target graph.
#![cfg(feature = "io")]

use std::collections::BTreeMap;

use graphersal::catalog::{Definition, PropertyIndexDefinition};
use graphersal::changes::Mutation;
use graphersal::io::{GraphImport, GraphSONImport};
use graphersal::prelude::*;
use graphersal::storage::{ElementKind, Graph, GraphStatistics, GraphStorage};

/// Every vertex `id -> (labels, properties)` and edge `id -> (label, from, to, properties)`.
type Snapshot = (
    BTreeMap<String, (Vec<String>, String)>,
    BTreeMap<String, (Option<String>, String, String, String)>,
);

fn snapshot(graph: &Graph) -> Snapshot {
    let g = graph.read();
    let mut vertices = BTreeMap::new();
    for v in g.vertices().unwrap() {
        let id = g.vertex_id(v).unwrap().unwrap().to_string();
        let labels = g
            .vertex_labels(v)
            .unwrap()
            .iter()
            .map(|l| l.to_string())
            .collect();
        let props = format!("{:?}", g.vertex_properties(v).unwrap());
        vertices.insert(id, (labels, props));
    }
    let mut edges = BTreeMap::new();
    for e in g.edges().unwrap() {
        let id = g.edge_id(e).unwrap().unwrap().to_string();
        let label = g.edge_label(e).unwrap().map(str::to_string);
        let (from, to) = g.edge_endpoints(e).unwrap().unwrap();
        let from = g.vertex_id(from).unwrap().unwrap().to_string();
        let to = g.vertex_id(to).unwrap().unwrap().to_string();
        let props = format!("{:?}", g.edge_properties(e).unwrap());
        edges.insert(id, (label, from, to, props));
    }
    (vertices, edges)
}

/// The graph's label counts equal a count over its elements, and its indexes are exact.
fn assert_consistent(graph: &Graph) {
    let g = graph.read();
    let kept = g.statistics().unwrap().unwrap().clone();
    let counted = GraphStatistics::from_storage(&*g).unwrap();
    assert_eq!(kept, counted, "label counts");
    g.check_property_indexes().unwrap();
}

fn load(text: &str) -> Graph {
    GraphSONImport::new()
        .load(std::io::Cursor::new(text))
        .expect("import")
        .0
}

/// Edges listed before their vertices (each edge in both directions, as our export and
/// TinkerPop write them), and the same graph with every vertex first.
const LATE_VERTICES: &str = concat!(
    r#"{"id":"a","label":"person","properties":{"name":[{"value":"ann"}]},"outE":{"knows":[{"id":"e1","inV":"b","properties":{"w":1}},{"id":"e2","inV":"c"}]}}"#,
    "\n",
    r#"{"id":"c","label":"city","properties":{"name":[{"value":"rome"}]},"inE":{"knows":[{"id":"e2","outV":"a"}],"lives":[{"id":"e3","outV":"b"}]}}"#,
    "\n",
    r#"{"id":"b","label":"person","properties":{"name":[{"value":"bob"}]},"inE":{"knows":[{"id":"e1","outV":"a","properties":{"w":1}}]},"outE":{"lives":[{"id":"e3","inV":"c"}]}}"#,
    "\n",
);

#[test]
fn edges_before_their_vertices_are_stored_through_stubs() {
    let graph = load(LATE_VERTICES);
    let (vertices, edges) = snapshot(&graph);
    assert_eq!(vertices.len(), 3);
    assert_eq!(edges.len(), 3);
    assert_eq!(vertices["b"].0, ["person"]);
    assert!(vertices["b"].1.contains("bob"), "{:?}", vertices["b"]);
    assert_eq!(edges["e1"].1, "a");
    assert_eq!(edges["e1"].2, "b");
    assert!(edges["e1"].3.contains("w"), "{:?}", edges["e1"]);
    assert_eq!(edges["e3"].1, "b");
    assert_consistent(&graph);
    // The label index knows the completed stubs.
    let people = graph.read().vertices_by_label("person").unwrap().count();
    assert_eq!(people, 2);
    // The same data as a graph built with every vertex first.
    let built = GraphSource::empty();
    {
        let mut g = built.write();
        let props = |name: &str| {
            let mut map = PropertyMap::default();
            map.insert("name", ElementProperty::from(name));
            map
        };
        let a = g
            .insert_vertex(Some("a".into()), vec!["person".into()], props("ann"))
            .unwrap();
        let c = g
            .insert_vertex(Some("c".into()), vec!["city".into()], props("rome"))
            .unwrap();
        let b = g
            .insert_vertex(Some("b".into()), vec!["person".into()], props("bob"))
            .unwrap();
        let mut w = PropertyMap::default();
        w.insert("w", ElementProperty::from(1i64));
        g.insert_edge(Some("e1".into()), Some("knows".into()), a, b, w)
            .unwrap();
        g.insert_edge(
            Some("e2".into()),
            Some("knows".into()),
            a,
            c,
            PropertyMap::default(),
        )
        .unwrap();
        g.insert_edge(
            Some("e3".into()),
            Some("lives".into()),
            b,
            c,
            PropertyMap::default(),
        )
        .unwrap();
    }
    assert_eq!(snapshot(&graph), snapshot(&built));
}

#[test]
fn a_completed_stub_is_captured_as_one_add_vertex() {
    let graph = GraphSource::empty();
    let ((), changes) = graph
        .write()
        .dry_run(|g| {
            GraphSONImport::new()
                .import_reader(g, LATE_VERTICES.as_bytes())
                .map(|_| ())
        })
        .unwrap();
    let mut added = BTreeMap::new();
    for mutation in changes.mutations() {
        match mutation {
            Mutation::AddVertex {
                id,
                labels,
                properties,
                ..
            } => {
                added.insert(
                    id.clone(),
                    (labels.clone(), properties.get("name").cloned()),
                );
            }
            Mutation::AddEdge { .. } => {}
            other => panic!("a stub must not show in the change set: {other:?}"),
        }
    }
    assert_eq!(added.len(), 3);
    assert_eq!(
        added["b"],
        (
            vec!["person".to_string()],
            Some(ElementProperty::from("bob"))
        )
    );
    assert_eq!(
        added["c"],
        (
            vec!["city".to_string()],
            Some(ElementProperty::from("rome"))
        )
    );
    // The dry run left nothing behind.
    assert_eq!(graph.read().vertex_count(), 0);
}

#[test]
fn a_failed_live_import_rolls_back_stubs_and_completions() {
    let mut graph = GraphSource::tinkerpop_modern();
    let before = snapshot(&graph);
    // `b` was a stub completed by line 2; line 3 is broken.
    let text = concat!(
        r#"{"id":"a","outE":{"knows":[{"id":"x1","inV":"b"},{"id":"x2","inV":"q"}]}}"#,
        "\n",
        r#"{"id":"b","label":"person","properties":{"name":[{"value":"bob"}]}}"#,
        "\n",
        "{broken\n",
    );
    assert!(graph.import_graphson_reader(text.as_bytes()).is_err());
    assert_eq!(snapshot(&graph), before);
    assert_consistent(&graph);
    // The same file with the broken line removed fails for the dangling edge to `q`, also
    // rolled back.
    let text = text.replace("{broken\n", "");
    assert!(matches!(
        graph.import_graphson_reader(text.as_bytes()),
        Err(IOError::DanglingEdges { count: 1, .. })
    ));
    assert_eq!(snapshot(&graph), before);
    assert_consistent(&graph);
}

/// A closed schema: `person` and `city` vertices, `knows` only between persons, with an
/// integer `w`.
fn closed_schema() -> GraphSchema {
    GraphSchema::from_json(
        r#"{"mode": "closed",
          "vertices": {
            "person": {"schema": {"type": "object", "properties": {"name": {"type": "string"}},
                                  "required": ["name"]}},
            "city": {"schema": {"type": "object", "properties": {"name": {"type": "string"}}}}},
          "edges": {"knows": {"schema": {"type": "object", "properties": {"w": {"type": "integer"}}},
                              "connections": [{"from": "person", "to": "person"}]}}}"#,
    )
    .expect("schema")
}

#[test]
fn a_closed_schema_checks_a_stub_when_it_is_completed() {
    let ok = concat!(
        r#"{"id":"a","label":"person","properties":{"name":[{"value":"ann"}]},"outE":{"knows":[{"id":"e1","inV":"b","properties":{"w":1}}]}}"#,
        "\n",
        r#"{"id":"b","label":"person","properties":{"name":[{"value":"bob"}]},"inE":{"knows":[{"id":"e1","outV":"a","properties":{"w":1}}]}}"#,
        "\n",
    );
    let mut graph = GraphSource::empty();
    graph.write().set_schema(closed_schema()).unwrap();
    graph.import_graphson_reader(ok.as_bytes()).unwrap();
    assert_eq!(graph.read().edge_count(), 1);
    assert_consistent(&graph);

    let mut graph = GraphSource::empty();
    graph.write().set_schema(closed_schema()).unwrap();
    // `knows` to a city: the connection is checked when the stub `c` is completed.
    let wrong_connection = concat!(
        r#"{"id":"a","label":"person","properties":{"name":[{"value":"ann"}]},"outE":{"knows":[{"id":"e1","inV":"c"}]}}"#,
        "\n",
        r#"{"id":"c","label":"city","properties":{"name":[{"value":"rome"}]}}"#,
        "\n",
    );
    let err = graph
        .import_graphson_reader(wrong_connection.as_bytes())
        .expect_err("knows connects persons only");
    assert!(err.to_string().contains("knows"), "{err}");
    assert_eq!(graph.read().vertex_count(), 0);
    // A stub's own record is validated like any vertex: `name` is required.
    let missing_required = concat!(
        r#"{"id":"a","label":"person","properties":{"name":[{"value":"ann"}]},"outE":{"knows":[{"id":"e1","inV":"b"}]}}"#,
        "\n",
        r#"{"id":"b","label":"person"}"#,
        "\n",
    );
    assert!(
        graph
            .import_graphson_reader(missing_required.as_bytes())
            .is_err()
    );
    // An edge to a stub still has its own properties checked at once.
    let wrong_property = concat!(
        r#"{"id":"a","label":"person","properties":{"name":[{"value":"ann"}]},"outE":{"knows":[{"id":"e1","inV":"b","properties":{"w":"heavy"}}]}}"#,
        "\n",
        r#"{"id":"b","label":"person","properties":{"name":[{"value":"bob"}]}}"#,
        "\n",
    );
    assert!(
        graph
            .import_graphson_reader(wrong_property.as_bytes())
            .is_err()
    );
    assert_eq!(graph.read().vertex_count(), 0);
    assert_consistent(&graph);
}

#[test]
fn a_unique_index_refuses_a_completed_stub_that_repeats_a_key() {
    let mut graph = GraphSource::empty();
    graph
        .write()
        .set_definition(Definition::property_index(
            "email",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "email").with_unique(true),
        ))
        .unwrap();
    graph
        .import_graphson_reader(
            r#"{"id":"x","label":"person","properties":{"email":[{"value":"e@x"}]}}"#.as_bytes(),
        )
        .unwrap();
    let text = concat!(
        r#"{"id":"a","outE":{"l":[{"id":"e1","inV":"b"}]}}"#,
        "\n",
        r#"{"id":"b","label":"person","properties":{"email":[{"value":"e@x"}]}}"#,
        "\n",
    );
    let err = graph
        .import_graphson_reader(text.as_bytes())
        .expect_err("a repeated unique key");
    assert!(
        matches!(err, IOError::Graph(GraphError::UniqueViolation { .. })),
        "{err}"
    );
    assert_eq!(graph.read().vertex_count(), 1);
    assert_consistent(&graph);
}

#[test]
fn the_second_copy_is_recognized_in_a_non_empty_target_graph() {
    let mut graph = GraphSource::tinkerpop_modern();
    let report = graph
        .import_graphson_reader(LATE_VERTICES.as_bytes())
        .unwrap();
    assert_eq!((report.vertices, report.edges), (3, 3));
    assert_eq!(
        (graph.read().vertex_count(), graph.read().edge_count()),
        (9, 9)
    );
    assert_consistent(&graph);
    // An edge id the target graph held before the import is refused, as before.
    let text = r#"{"id":"n","outE":{"knows":[{"id":"0","inV":"1"}]}}"#;
    assert!(matches!(
        graph.import_graphson_reader(text.as_bytes()),
        Err(IOError::Graph(GraphError::EdgeIdUniqueConstraint { .. }))
    ));
}

#[test]
fn an_edge_without_an_id_whose_in_copy_comes_first_is_stored_once() {
    // `b` lists the edge in its inE before `a`'s record lists it in its outE.
    let text = concat!(
        r#"{"id":"b","inE":{"l":[{"outV":"a"}]}}"#,
        "\n",
        r#"{"id":"a","outE":{"l":[{"inV":"b"}]}}"#,
        "\n",
        r#"{"id":"c","inE":{"m":[{"outV":"d"}]}}"#,
        "\n",
        r#"{"id":"d"}"#,
        "\n",
    );
    let graph = load(text);
    // a->b once (the outE copy of a is recognized), d->c from the inE only (d lists no outE).
    assert_eq!(graph.read().edge_count(), 2);
    assert_consistent(&graph);
}

#[test]
fn graphml_edges_before_their_nodes_are_stored_through_stubs() {
    let late = r#"<graphml>
      <key id="name" for="node" attr.name="name" attr.type="string"/>
      <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
      <key id="labelE" for="edge" attr.name="labelE" attr.type="string"/>
      <graph edgedefault="directed">
        <edge id="e1" source="a" target="b"><data key="labelE">knows</data></edge>
        <node id="a"><data key="labelV">person</data><data key="name">ann</data></node>
        <node id="b"><data key="labelV">person</data><data key="name">bob</data></node>
      </graph></graphml>"#;
    let graph = GraphSource::from_graphml_reader(std::io::Cursor::new(late)).unwrap();
    let (vertices, edges) = snapshot(&graph);
    assert_eq!(vertices["b"].0, ["person"]);
    assert!(vertices["b"].1.contains("bob"));
    assert_eq!(
        edges["e1"],
        (
            Some("knows".to_string()),
            "a".to_string(),
            "b".to_string(),
            edges["e1"].3.clone()
        )
    );
    assert_consistent(&graph);
    // A node that never comes fails the import.
    let dangling =
        r#"<graphml><graph><node id="a"/><edge source="a" target="zz"/></graph></graphml>"#;
    match GraphSource::from_graphml_reader(std::io::Cursor::new(dangling)) {
        Err(IOError::DanglingEdges {
            vertices, count, ..
        }) => assert_eq!((count, vertices), (1, vec!["zz".to_string()])),
        other => panic!("expected DanglingEdges, got {other:?}"),
    }
}
