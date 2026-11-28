//! GraphML import/export edge cases: `for="edge"`/`for="all"` keys and their defaults,
//! `attr.type="float"`, graph-level data, edges without an id,
//! every malformed `<key>`/`<edge>` attribute, file errors, key-id collisions between vertex and
//! edge properties on export, and non-finite floats inside a container.

#![cfg(feature = "io")]

use graphersal::io::{GraphExport, GraphImport};
use graphersal::prelude::*;

fn import(xml: &str) -> Result<Graph, IOError> {
    let mut g = GraphSource::empty();
    g.import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()))?;
    Ok(g)
}

fn export(graph: &Graph) -> String {
    let mut out = Vec::new();
    graph.export_graphml_writer(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn element_maps(graph: &Graph, edges: bool) -> Vec<ElementProperty> {
    let lock = graph.read();
    let g = lock.traversal();
    let source = if edges {
        g.e(None::<()>)
    } else {
        g.v(None::<()>)
    };
    source.element_map(None).to_list().unwrap()
}

fn get<'a>(map: &'a ElementProperty, key: &str) -> Option<&'a ElementProperty> {
    map.as_object().and_then(|o| o.get(key))
}

const DEFAULTS: &str = r#"<?xml version="1.0"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="gk" for="graph" attr.name="title" attr.type="string"/>
  <key id="w" for="edge" attr.name="w" attr.type="float"><default>0.5</default></key>
  <key id="c" for="all" attr.name="color" attr.type="string"><default>red</default></key>
  <key id="name" for="node" attr.name="name" attr.type="string"/>
  <graph id="G" edgedefault="directed">
    <data key="gk">My graph</data>
    <node id="a"><data key="name">A</data></node>
    <node id="b"/>
    <edge source="a" target="b"/>
    <edge id="" source="b" target="a"><data key="w">2.25</data><data key="c">blue</data></edge>
  </graph>
</graphml>"#;

#[test]
fn edge_and_all_key_defaults_fill_missing_data() {
    let graph = import(DEFAULTS).unwrap();
    let vertices = element_maps(&graph, false);
    assert_eq!(vertices.len(), 2);
    for v in &vertices {
        // `for="all"` applies to nodes; `for="edge"` does not
        assert_eq!(get(v, "color"), Some(&"red".into()));
        assert!(get(v, "w").is_none());
        // graph-level data is not a vertex property
        assert!(get(v, "title").is_none());
    }
    let edges = element_maps(&graph, true);
    assert_eq!(edges.len(), 2);
    let (first, second) = (&edges[0], &edges[1]);
    assert_eq!(get(first, "w"), Some(&0.5.into()), "edge default");
    assert_eq!(get(first, "color"), Some(&"red".into()), "all default");
    assert_eq!(get(second, "w"), Some(&2.25.into()), "float attr.type");
    assert_eq!(
        get(second, "color"),
        Some(&"blue".into()),
        "explicit data wins"
    );
}

#[test]
fn edges_without_or_with_an_empty_id_get_assigned_ids() {
    let graph = import(DEFAULTS).unwrap();
    let lock = graph.read();
    let ids = lock.traversal().e(None::<()>).id().to_list().unwrap();
    assert_eq!(ids.len(), 2);
    assert!(
        ids.iter()
            .all(|id| id.as_str().is_some_and(|s| !s.is_empty()))
    );
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn every_missing_key_attribute_is_named() {
    for (attrs, missing) in [
        (r#"for="node" attr.name="n" attr.type="string""#, "id"),
        (r#"id="k" attr.name="n" attr.type="string""#, "for"),
        (r#"id="k" for="node" attr.type="string""#, "attr.name"),
        (r#"id="k" for="node" attr.name="n""#, "attr.type"),
    ] {
        let xml = format!(r#"<graphml><key {attrs}/><graph/></graphml>"#);
        match import(&xml).err() {
            Some(IOError::MissingAttribute {
                element, attribute, ..
            }) => {
                assert_eq!((element.as_str(), attribute.as_str()), ("key", missing));
            }
            other => panic!("{attrs}: expected MissingAttribute, got {other:?}"),
        }
    }
}

#[test]
fn an_unknown_key_target_is_rejected() {
    let xml =
        r#"<graphml><key id="k" for="hyperedge" attr.name="n" attr.type="string"/></graphml>"#;
    match import(xml).err() {
        Some(IOError::InvalidAttributeValue {
            key,
            attribute,
            value,
            expected_type,
            ..
        }) => {
            assert_eq!(key, "k");
            assert_eq!(attribute, "for");
            assert_eq!(value, "hyperedge");
            assert_eq!(expected_type, "graph|node|edge|all");
        }
        other => panic!("expected InvalidAttributeValue, got {other:?}"),
    }
}

#[test]
fn an_edge_without_a_source_or_with_an_unknown_one_fails() {
    let xml = r#"<graphml><graph><node id="1"/><edge target="1"/></graph></graphml>"#;
    match import(xml).err() {
        Some(IOError::MissingAttribute {
            element, attribute, ..
        }) => {
            assert_eq!((element.as_str(), attribute.as_str()), ("edge", "source"));
        }
        other => panic!("expected MissingAttribute, got {other:?}"),
    }
    let xml = r#"<graphml><graph><node id="1"/><edge source="nope" target="1"/></graph></graphml>"#;
    match import(xml).err() {
        Some(IOError::DanglingEdges { vertices, .. }) => assert_eq!(vertices, ["nope"]),
        other => panic!("expected DanglingEdges, got {other:?}"),
    }
}

#[test]
fn file_errors_carry_the_path() {
    let missing = "/nonexistent-graphersal-dir/in.xml";
    let mut graph = GraphSource::empty();
    match graph.import_graphml(missing) {
        Err(IOError::FileRead { path, .. }) => assert_eq!(path, missing),
        other => panic!("expected FileRead, got {other:?}"),
    }
    let target = "/nonexistent-graphersal-dir/out.xml";
    match GraphSource::tinkerpop_modern().export_graphml(target) {
        Err(IOError::FileRead { path, .. }) => assert_eq!(path, target),
        other => panic!("expected an error for the unwritable path, got {other:?}"),
    }
}

#[test]
fn beautified_export_of_a_shared_graph_is_indented_and_reimports_equal() {
    let graph = GraphSource::tinkerpop_modern();
    let mut out = Vec::new();
    graph
        .export_graphml_writer_with_options(&mut out, true)
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("\n  <key "), "indented: {text}");
    let back = import(&text).unwrap();
    assert_eq!(element_maps(&back, false), element_maps(&graph, false));
    assert_eq!(element_maps(&back, true).len(), 6);
}

#[test]
fn an_edge_property_named_like_a_vertex_property_gets_its_own_key() {
    let graph = GraphSource::empty();
    graph
        .write()
        .traversal_mut()
        .add_v("p")
        .property("name", "a")
        .property("labelE", "not an edge label")
        .as_("a")
        .add_v("p")
        .property("name", "b")
        .add_e("rel")
        .from("a")
        .property("name", 7)
        .to_list()
        .unwrap();
    let text = export(&graph);
    // the vertex `name` key keeps its id, the edge one is prefixed; same for the edge label key
    assert!(text.contains(r#"<key id="name" for="node""#), "{text}");
    assert!(text.contains(r#"<key id="e_name" for="edge""#), "{text}");
    assert!(text.contains(r#"<key id="labelE" for="node""#), "{text}");
    assert!(text.contains(r#"<key id="e_labelE" for="edge""#), "{text}");
    let back = import(&text).unwrap();
    let edges = element_maps(&back, true);
    assert_eq!(get(&edges[0], "name"), Some(&7.into()));
    assert_eq!(get(&edges[0], "label"), Some(&"rel".into()));
    let vertices = element_maps(&back, false);
    assert!(
        vertices
            .iter()
            .any(|v| get(v, "labelE") == Some(&"not an edge label".into()))
    );
}

#[test]
fn a_non_finite_float_inside_a_container_cannot_be_exported() {
    let graph = GraphSource::empty();
    graph
        .write()
        .traversal_mut()
        .add_v("p")
        .property("scores", vec![1.0, f64::NAN])
        .to_list()
        .unwrap();
    let mut out = Vec::new();
    match graph.export_graphml_writer(&mut out) {
        Err(IOError::NonFiniteFloat { key, .. }) => assert_eq!(key, "scores"),
        other => panic!("expected NonFiniteFloat, got {other:?}"),
    }
}

#[test]
fn elements_without_data_export_as_empty_tags() {
    let graph = GraphSource::empty();
    {
        let mut lock = graph.write();
        let a =
            GraphStorage::add_vertex(&mut *lock, Some("a".into()), vec![], PropertyMap::default())
                .unwrap();
        let b =
            GraphStorage::add_vertex(&mut *lock, Some("b".into()), vec![], PropertyMap::default())
                .unwrap();
        GraphStorage::add_edge(
            &mut *lock,
            Some("e".into()),
            None,
            a,
            b,
            PropertyMap::default(),
        )
        .unwrap();
    }
    let text = export(&graph);
    assert!(text.contains(r#"<node id="a"/>"#), "{text}");
    assert!(
        text.contains(r#"<edge id="e" source="a" target="b"/>"#),
        "{text}"
    );
    let back = import(&text).unwrap();
    assert_eq!(back.read().vertex_count(), 2);
    assert_eq!(back.read().edge_count(), 1);
}
