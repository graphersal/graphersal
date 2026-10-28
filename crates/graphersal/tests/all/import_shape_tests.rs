//! The internal importers (GraphSON, GraphML, `apply_change_set` / `replay`) build the stored
//! property maps directly in the graph's shapes: observable behaviour pinned here is property
//! order, repeated keys (first position, last value), schema coercion, compression rules,
//! property indexes, change capture, sharing of the graph's existing shapes, and rollback of a
//! failed import.
#![cfg(feature = "io")]

use std::cell::RefCell;

use graphersal::catalog::{CompressionDefinition, PropertyIndexDefinition};
use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::io::{GraphImport, GraphSONImport};
use graphersal::prelude::*;
use graphersal::prelude::{Graph, PropertyMap};
use graphersal::storage::{ElementKind, PropertyIndexRange};

/// The properties of vertex `id` in stored order.
fn vertex_entries(graph: &TraversalGraph, id: &str) -> Vec<(String, ElementProperty)> {
    let v = graph.get_vertex_by_id(id).unwrap().expect("vertex");
    graph
        .vertex_properties(v)
        .unwrap()
        .expect("live")
        .iter()
        .map(|(k, val)| (k.to_string(), val.to_logical()))
        .collect()
}

/// The properties of edge `id` in stored order.
fn edge_entries(graph: &TraversalGraph, id: &str) -> Vec<(String, ElementProperty)> {
    let e = graph.get_edge_by_id(id).unwrap().expect("edge");
    graph
        .edge_properties(e)
        .unwrap()
        .expect("live")
        .iter()
        .map(|(k, val)| (k.to_string(), val.to_logical()))
        .collect()
}

fn entries(list: &[(&str, ElementProperty)]) -> Vec<(String, ElementProperty)> {
    list.iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// Every vertex and edge with labels and properties (in stored order), sorted by id.
fn content(graph: &TraversalGraph) -> Vec<String> {
    let mut out = Vec::new();
    for v in GraphStorage::vertices(graph).unwrap() {
        let id = graph.vertex_id(v).unwrap().unwrap_or_default().to_string();
        out.push(format!(
            "v {id} {:?} {:?}",
            graph.vertex_labels(v).unwrap(),
            vertex_entries(graph, &id)
        ));
    }
    for e in GraphStorage::edges(graph).unwrap() {
        let id = graph.edge_id(e).unwrap().unwrap_or_default().to_string();
        out.push(format!(
            "e {id} {:?} {:?}",
            graph.edge_label(e).unwrap(),
            edge_entries(graph, &id)
        ));
    }
    out.sort();
    out
}

fn graphson(text: &str) -> TraversalGraph {
    let (graph, _) = GraphSONImport::new().load(text.as_bytes()).expect("import");
    graph_of(graph)
}

fn graph_of(graph: Graph) -> TraversalGraph {
    std::mem::take(&mut *graph.write())
}

fn graphml(xml: &str) -> TraversalGraph {
    graph_of(GraphSource::from_graphml_reader(xml.as_bytes()).expect("import"))
}

fn int(n: i64) -> ElementProperty {
    ElementProperty::from(n)
}

fn text(s: &str) -> ElementProperty {
    ElementProperty::from(s)
}

// ---------------------------------------------------------------------------------------------
// GraphSON
// ---------------------------------------------------------------------------------------------

const GRAPHSON_ORDER: &str = concat!(
    r#"{"id":"a","label":"p","properties":{"z":[{"value":1}],"a":[{"value":2}],"m":[{"value":3}]},"#,
    r#""outE":{"e":[{"id":"e1","inV":"b","properties":{"w":1,"c":2,"b":3}}]}}"#,
    "\n",
    r#"{"id":"b","label":"p","properties":{"m":[{"value":4}],"z":[{"value":5}]}}"#,
    "\n",
    r#"{"id":"c","label":"p","properties":{"z":[{"value":6}],"a":[{"value":7}],"m":[{"value":8}]}}"#,
    "\n",
);

#[test]
fn graphson_keeps_the_property_order_of_the_file() {
    let graph = graphson(GRAPHSON_ORDER);
    assert_eq!(
        vertex_entries(&graph, "a"),
        entries(&[("z", int(1)), ("a", int(2)), ("m", int(3))])
    );
    assert_eq!(
        vertex_entries(&graph, "b"),
        entries(&[("m", int(4)), ("z", int(5))])
    );
    assert_eq!(
        vertex_entries(&graph, "c"),
        entries(&[("z", int(6)), ("a", int(7)), ("m", int(8))])
    );
    assert_eq!(
        edge_entries(&graph, "e1"),
        entries(&[("w", int(1)), ("c", int(2)), ("b", int(3))])
    );
}

#[test]
fn graphson_repeated_keys_keep_the_first_position_and_the_last_value() {
    let text = concat!(
        r#"{"id":"a","label":"p","properties":{"k":[{"value":1}],"j":[{"value":2}],"k":[{"value":3}]},"#,
        r#""outE":{"e":[{"id":"e1","inV":"b","properties":{"x":1,"y":2,"x":3}}]}}"#,
        "\n",
        // The repeat cannot be decoded (a skipped type): the first value stays.
        r#"{"id":"b","label":"p","properties":{"k":[{"value":1}],"j":[{"value":2}],"k":[{"value":{"@type":"gx:Year","@value":"2020"}}]}}"#,
        "\n",
    );
    let (graph, report) = GraphSONImport::new().load(text.as_bytes()).unwrap();
    assert!(report.has_skips());
    let graph = graph_of(graph);
    assert_eq!(
        vertex_entries(&graph, "a"),
        entries(&[("k", int(3)), ("j", int(2))])
    );
    assert_eq!(
        vertex_entries(&graph, "b"),
        entries(&[("k", int(1)), ("j", int(2))])
    );
    assert_eq!(
        edge_entries(&graph, "e1"),
        entries(&[("x", int(3)), ("y", int(2))])
    );
}

#[test]
fn graphson_meta_properties_go_last_into_meta() {
    let text = r#"{"id":"a","label":"p","properties":{"name":[{"value":"x","properties":{"since":1}}],"age":[{"value":2}]}}"#;
    let graph = graphson(text);
    let keys: Vec<String> = vertex_entries(&graph, "a")
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(keys, ["name", "age", "_meta"]);
}

/// A closed schema: `person` with `name: string`, `id_card: uuid`.
fn person_schema() -> GraphSchema {
    GraphSchema::from_json(
        r#"{"mode": "closed", "vertices": {"person": {"schema": {"type": "object", "properties": {
            "name": {"type": "string"},
            "id_card": {"type": "string", "format": "uuid"}
        }}}}}"#,
    )
    .expect("schema")
}

const UUID_TEXT: &str = "41d2e28a-20a4-4ab0-b379-d810dede3786";

#[test]
fn imports_and_change_sets_coerce_against_the_schema() {
    // GraphSON (live import).
    let mut graph = TraversalGraph::new();
    graph.set_schema(person_schema()).unwrap();
    let line = format!(
        r#"{{"id":"p","label":"person","properties":{{"name":[{{"value":"x"}}],"id_card":[{{"value":"{UUID_TEXT}"}}]}}}}"#
    );
    graph.import_graphson_reader(line.as_bytes()).unwrap();
    assert!(matches!(
        vertex_entries(&graph, "p")[1].1,
        ElementProperty::Uuid(_)
    ));

    // A change set of free-standing maps.
    let mut target = TraversalGraph::new();
    target.set_schema(person_schema()).unwrap();
    let mut builder = ChangeSet::builder(1, 1);
    builder.push(Mutation::add_vertex(
        "p",
        ["person"],
        PropertyMap::from([("name", text("x")), ("id_card", text(UUID_TEXT))]),
    ));
    target.apply_change_set(&builder.build().unwrap()).unwrap();
    assert_eq!(
        vertex_entries(&target, "p"),
        vertex_entries(&graph, "p"),
        "coerced like the import"
    );

    // A violation still fails the whole import and leaves nothing.
    let bad = format!(
        r#"{{"id":"q","label":"person","properties":{{"id_card":[{{"value":"{UUID_TEXT}"}}]}}}}
{{"id":"r","label":"person","properties":{{"id_card":[{{"value":"not a uuid"}}]}}}}"#
    );
    assert!(graph.import_graphson_reader(bad.as_bytes()).is_err());
    assert!(graph.get_vertex_by_id("q").unwrap().is_none());
}

// ---------------------------------------------------------------------------------------------
// GraphML
// ---------------------------------------------------------------------------------------------

const GRAPHML_HEAD: &str = r#"<?xml version="1.0"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
  <key id="labelE" for="edge" attr.name="labelE" attr.type="string"/>
  <key id="z" for="node" attr.name="z" attr.type="long"/>
  <key id="a" for="node" attr.name="a" attr.type="long"/>
  <key id="m" for="node" attr.name="m" attr.type="long"/>
  <key id="w" for="edge" attr.name="w" attr.type="long"/>
  <key id="c" for="edge" attr.name="c" attr.type="long"/>
  <graph id="G" edgedefault="directed">
"#;

const GRAPHML_TAIL: &str = "  </graph>\n</graphml>\n";

#[test]
fn graphml_keeps_the_data_order_and_merges_repeats() {
    let xml = format!(
        r#"{GRAPHML_HEAD}
    <node id="1"><data key="z">1</data><data key="labelV">p</data><data key="a">2</data><data key="m">3</data></node>
    <node id="2"><data key="m">4</data><data key="z">5</data><data key="m">6</data><data key="labelV">x</data><data key="labelV">q</data></node>
    <edge id="e1" source="1" target="2"><data key="c">1</data><data key="labelE">e</data><data key="w">2</data><data key="c">3</data></edge>
{GRAPHML_TAIL}"#
    );
    let graph = graphml(&xml);
    assert_eq!(
        vertex_entries(&graph, "1"),
        entries(&[("z", int(1)), ("a", int(2)), ("m", int(3))])
    );
    // A repeated key: first position, last value; a repeated `labelV`: the last one.
    assert_eq!(
        vertex_entries(&graph, "2"),
        entries(&[("m", int(6)), ("z", int(5))])
    );
    let v2 = graph.get_vertex_by_id("2").unwrap().unwrap();
    assert_eq!(graph.vertex_labels(v2).unwrap(), ["q"]);
    assert_eq!(
        edge_entries(&graph, "e1"),
        entries(&[("c", int(3)), ("w", int(2))])
    );
    let e1 = graph.get_edge_by_id("e1").unwrap().unwrap();
    assert_eq!(graph.edge_label(e1).unwrap(), Some("e"));
}

#[test]
fn graphml_defaults_fill_absent_keys_after_the_given_ones() {
    let xml = r#"<?xml version="1.0"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="a" for="node" attr.name="a" attr.type="long"><default>9</default></key>
  <key id="b" for="node" attr.name="b" attr.type="long"/>
  <graph id="G" edgedefault="directed">
    <node id="1"><data key="b">1</data></node>
    <node id="2"><data key="a">2</data><data key="b">3</data></node>
  </graph>
</graphml>
"#;
    let graph = graphml(xml);
    assert_eq!(
        vertex_entries(&graph, "1"),
        entries(&[("b", int(1)), ("a", int(9))])
    );
    assert_eq!(
        vertex_entries(&graph, "2"),
        entries(&[("a", int(2)), ("b", int(3))])
    );
}

// ---------------------------------------------------------------------------------------------
// Compression rules, property indexes
// ---------------------------------------------------------------------------------------------

fn long_text(i: usize) -> String {
    format!("{i} {}", "lorem ipsum dolor sit amet ".repeat(40))
}

/// A graph with a compression rule on `p.bio` and an index on `p.age`.
fn ruled_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    {
        let mut g = graph.traversal_mut();
        g.define_compression(
            "bio",
            CompressionDefinition::new(ElementKind::Vertex, "p", "bio").unwrap(),
        )
        .unwrap();
        g.create_index(
            "age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "p", "age"),
        )
        .unwrap();
    }
    graph
}

fn ages(graph: &TraversalGraph, age: i64) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .vertices_by_property_index("p", "age", &PropertyIndexRange::equal(int(age)))
        .unwrap()
        .expect("an index")
        .filter_map(|v| graph.vertex_id(v).unwrap().map(str::to_string))
        .collect();
    ids.sort();
    ids
}

fn check_ruled(graph: &TraversalGraph, how: &str) {
    assert_eq!(
        graph.memory_usage().unwrap().unwrap().compressed_values,
        3,
        "{how}"
    );
    graph.check_compression_invariant().unwrap();
    graph.check_property_indexes().unwrap();
    assert_eq!(ages(graph, 7), ["0", "2"], "{how}");
    assert_eq!(ages(graph, 8), ["1"], "{how}");
    assert_eq!(
        vertex_entries(graph, "1"),
        entries(&[("bio", text(&long_text(1))), ("age", int(8))]),
        "{how}"
    );
}

#[test]
fn compression_rules_and_property_indexes_apply_to_imported_data() {
    let lines: String = (0..3)
        .map(|i| {
            format!(
                r#"{{"id":"{i}","label":"p","properties":{{"bio":[{{"value":"{}"}}],"age":[{{"value":{}}}]}}}}"#,
                long_text(i),
                if i == 1 { 8 } else { 7 }
            ) + "\n"
        })
        .collect();
    let mut graph = ruled_graph();
    graph.import_graphson_reader(lines.as_bytes()).unwrap();
    check_ruled(&graph, "GraphSON");

    let nodes: String = (0..3)
        .map(|i| {
            format!(
                r#"<node id="{i}"><data key="labelV">p</data><data key="bio">{}</data><data key="age">{}</data></node>"#,
                long_text(i),
                if i == 1 { 8 } else { 7 }
            )
        })
        .collect();
    let xml = format!(
        r#"<?xml version="1.0"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="labelV" for="node" attr.name="labelV" attr.type="string"/>
  <key id="bio" for="node" attr.name="bio" attr.type="string"/>
  <key id="age" for="node" attr.name="age" attr.type="long"/>
  <graph id="G" edgedefault="directed">{nodes}</graph>
</graphml>
"#
    );
    let mut graph = ruled_graph();
    graph.import_graphml_reader(xml.as_bytes()).unwrap();
    check_ruled(&graph, "GraphML");

    let mut builder = ChangeSet::builder(1, 1);
    for i in 0..3 {
        builder.push(Mutation::add_vertex(
            i.to_string(),
            ["p"],
            PropertyMap::from([
                ("bio", text(&long_text(i))),
                ("age", int(if i == 1 { 8 } else { 7 })),
            ]),
        ));
    }
    let changes = builder.build().unwrap();
    let mut graph = ruled_graph();
    graph.apply_change_set(&changes).unwrap();
    check_ruled(&graph, "apply_change_set");
    let graph = ruled_graph().replay(&changes).unwrap();
    check_ruled(&graph, "replay");
}

// ---------------------------------------------------------------------------------------------
// Change capture
// ---------------------------------------------------------------------------------------------

thread_local! {
    static RECORDED: RefCell<Vec<ChangeSet>> = const { RefCell::new(Vec::new()) };
}

struct Recorder;

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        RECORDED.with(|recorded| recorded.borrow_mut().push(changes.clone()));
        Ok(())
    }
}

fn recorded() -> Vec<ChangeSet> {
    RECORDED.with(|recorded| std::mem::take(&mut *recorded.borrow_mut()))
}

#[test]
fn an_import_is_captured_in_order_and_its_change_set_rebuilds_the_graph() {
    let mut graph = TraversalGraph::new();
    graph.register_commit_hook(Box::new(Recorder)).unwrap();
    graph
        .import_graphson_reader(GRAPHSON_ORDER.as_bytes())
        .unwrap();
    let sets = recorded();
    assert_eq!(sets.len(), 1);
    let added: Vec<(String, Vec<String>)> = sets[0]
        .mutations()
        .iter()
        .filter_map(|m| match m {
            Mutation::AddVertex { id, properties, .. } => Some((
                id.clone(),
                properties.keys().map(|k| k.to_string()).collect(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        added,
        [
            ("a".to_string(), vec!["z".into(), "a".into(), "m".into()]),
            ("b".to_string(), vec!["m".into(), "z".into()]),
            ("c".to_string(), vec!["z".into(), "a".into(), "m".into()]),
        ]
    );
    // The captured change set (maps of the graph) and a decoded copy (free-standing maps).
    let mut copy = TraversalGraph::new();
    copy.apply_change_set(&sets[0]).unwrap();
    assert_eq!(content(&copy), content(&graph));
    let mut builder = ChangeSet::builder(1, 1);
    for mutation in sets[0].mutations() {
        builder.push(match mutation {
            Mutation::AddVertex {
                id,
                labels,
                properties,
                ..
            } => Mutation::add_vertex(
                id.clone(),
                labels.clone(),
                properties
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
            other => other.clone(),
        });
    }
    let replayed = TraversalGraph::new()
        .replay(&builder.build().unwrap())
        .unwrap();
    assert_eq!(content(&replayed), content(&graph));
}

// ---------------------------------------------------------------------------------------------
// Existing shapes, rollback
// ---------------------------------------------------------------------------------------------

#[test]
fn a_live_import_shares_the_shapes_the_graph_already_has() {
    let mut graph = TraversalGraph::new();
    for i in 0..3 {
        let props = PropertyMap::from([("z", int(i)), ("a", int(i)), ("m", int(i))]);
        graph
            .insert_vertex(Some(format!("old{i}").into()), vec!["p".into()], props)
            .unwrap();
    }
    let before = graph.memory_usage().unwrap().unwrap().shape_bytes;
    let lines: String = (0..50)
        .map(|i| {
            format!(r#"{{"id":"n{i}","label":"p","properties":{{"z":[{{"value":{i}}}],"a":[{{"value":1}}],"m":[{{"value":2}}]}}}}"#) + "\n"
        })
        .collect();
    graph.import_graphson_reader(lines.as_bytes()).unwrap();
    assert_eq!(graph.memory_usage().unwrap().unwrap().shape_bytes, before);
    // The same through a change set and a GraphML import.
    let mut builder = ChangeSet::builder(1, 1);
    builder.push(Mutation::add_vertex(
        "cs",
        ["p"],
        PropertyMap::from([("z", int(1)), ("a", int(2)), ("m", int(3))]),
    ));
    graph.apply_change_set(&builder.build().unwrap()).unwrap();
    let xml = format!(
        r#"{GRAPHML_HEAD}<node id="ml"><data key="z">1</data><data key="a">2</data><data key="m">3</data></node>{GRAPHML_TAIL}"#
    );
    graph.import_graphml_reader(xml.as_bytes()).unwrap();
    assert_eq!(graph.memory_usage().unwrap().unwrap().shape_bytes, before);
    assert_eq!(GraphStorage::vertices(&graph).unwrap().count(), 55);
}

#[test]
fn a_failed_import_or_change_set_leaves_nothing() {
    let mut graph = graphson(GRAPHSON_ORDER);
    let before = content(&graph);
    let stats = format!("{:?}", graph.statistics());
    let shapes = graph.memory_usage().unwrap().unwrap().shape_bytes;

    // GraphSON: new keys first, then an id the graph already has.
    let bad = concat!(
        r#"{"id":"n1","label":"q","properties":{"new":[{"value":1}],"other":[{"value":2}]}}"#,
        "\n",
        r#"{"id":"a","label":"p","properties":{"z":[{"value":1}]}}"#,
        "\n",
    );
    assert!(graph.import_graphson_reader(bad.as_bytes()).is_err());
    assert_eq!(content(&graph), before);

    // GraphML: the same.
    let xml = format!(
        r#"{GRAPHML_HEAD}<node id="n2"><data key="m">1</data><data key="a">2</data></node><node id="b"><data key="z">1</data></node>{GRAPHML_TAIL}"#
    );
    assert!(graph.import_graphml_reader(xml.as_bytes()).is_err());
    assert_eq!(content(&graph), before);

    // A change set whose last mutation fails.
    let mut builder = ChangeSet::builder(1, 1);
    builder.push(Mutation::add_vertex(
        "n3",
        ["q"],
        PropertyMap::from([("x", int(1)), ("y", int(2))]),
    ));
    builder.push(Mutation::add_vertex("c", ["p"], PropertyMap::new()));
    assert!(graph.apply_change_set(&builder.build().unwrap()).is_err());
    assert_eq!(content(&graph), before);

    assert_eq!(format!("{:?}", graph.statistics()), stats);
    graph.shrink_to_fit();
    assert_eq!(graph.memory_usage().unwrap().unwrap().shape_bytes, shapes);
}
