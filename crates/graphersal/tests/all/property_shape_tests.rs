//! Shared key lists of element properties ("shapes"), observed through the public API: the
//! memory figures (`shape_bytes`, counted once per distinct key sequence and part of
//! `property_bytes`), their release by `shrink_to_fit`, and maps handed out by the graph behaving
//! like any other map (equality, `Debug`, mutation of a copy).

use graphersal::prelude::PropertyMap;
use graphersal::prelude::*;

use crate::script_host::eval_value;

fn person(
    graph: &mut TraversalGraph,
    id: &str,
    keys: &[&str],
) -> graphersal::storage::VertexHandle {
    let props: PropertyMap = keys
        .iter()
        .enumerate()
        .map(|(i, k)| (*k, ElementProperty::from(i as i64)))
        .collect();
    graph
        .insert_vertex(Some(id.into()), vec!["person".into()], props)
        .unwrap()
}

#[test]
fn shape_bytes_do_not_grow_with_elements_of_the_same_keys() {
    let mut one = TraversalGraph::new();
    person(&mut one, "0", &["name", "age", "city"]);
    let mut many = TraversalGraph::new();
    for i in 0..500 {
        person(&mut many, &i.to_string(), &["name", "age", "city"]);
    }
    let one = one.memory_usage().unwrap().unwrap();
    let many_stats = many.memory_usage().unwrap().unwrap();
    assert!(one.shape_bytes > 0);
    assert_eq!(one.shape_bytes, many_stats.shape_bytes);
    assert!(many_stats.property_bytes >= many_stats.shape_bytes);
    // A slot is one value: three small numbers per vertex, no key bytes per element.
    let per_vertex = (many_stats.property_bytes - many_stats.shape_bytes) / 500;
    assert_eq!(per_vertex, 3 * 32);
}

#[test]
fn a_new_key_order_is_a_new_shape_and_shrink_releases_unused_ones() {
    let mut graph = TraversalGraph::new();
    person(&mut graph, "a", &["name", "age"]);
    let base = graph.memory_usage().unwrap().unwrap().shape_bytes;
    let b = person(&mut graph, "b", &["age", "name"]);
    let two = graph.memory_usage().unwrap().unwrap().shape_bytes;
    assert!(two > base, "{two} vs {base}");
    graph.drop_vertex(b).unwrap();
    graph.shrink_to_fit();
    assert!(graph.memory_usage().unwrap().unwrap().shape_bytes < two);
}

#[test]
fn the_rhai_memory_map_reports_shape_bytes() {
    let graph = std::sync::Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
    let stats = graph.read().memory_usage().unwrap().unwrap();
    let reported = eval_value(graph.clone(), "g.memory_usage().shape_bytes")
        .unwrap()
        .as_int()
        .unwrap();
    assert_eq!(reported as u64, stats.shape_bytes);
    assert!(stats.shape_bytes > 0);
}

#[test]
fn a_stored_map_handed_out_behaves_like_any_map() {
    let mut graph = TraversalGraph::new();
    let a = person(&mut graph, "a", &["name", "age"]);
    let stored = graph.vertex_properties(a).unwrap().unwrap().clone();
    let built = PropertyMap::from([
        ("age", ElementProperty::from(1)),
        ("name", ElementProperty::from(0)),
    ]);
    assert_eq!(stored, built);
    assert_eq!(built, stored);
    assert_eq!(format!("{stored:?}"), r#"{"name": 0, "age": 1}"#);
    // Changing a copy leaves the graph alone.
    let mut copy = stored.clone();
    copy.insert("city", ElementProperty::from("rome"));
    copy.shift_remove("name");
    assert_eq!(
        copy.keys().map(|k| k.as_str()).collect::<Vec<_>>(),
        ["age", "city"]
    );
    assert_eq!(graph.vertex_properties(a).unwrap().unwrap(), &stored);
    // Inserting the changed copy into another element shares that element's key order.
    let c = graph
        .insert_vertex(Some("c".into()), vec![], copy.clone())
        .unwrap();
    assert_eq!(graph.vertex_properties(c).unwrap().unwrap(), &copy);
    assert_eq!(
        graph
            .vertex_properties(c)
            .unwrap()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect::<Vec<_>>(),
        ["age", "city"]
    );
}

#[test]
fn wide_elements_read_every_key() {
    let mut graph = TraversalGraph::new();
    let keys: Vec<String> = (0..40).map(|i| format!("field_{i}")).collect();
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let a = person(&mut graph, "a", &refs);
    let b = person(&mut graph, "b", &refs);
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(
            graph.vertex_properties(a).unwrap().unwrap().get(key),
            Some(&ElementProperty::from(i as i64))
        );
    }
    graph.drop_vertex_property("field_7", b).unwrap();
    graph
        .set_vertex_property(b, "field_7".into(), ElementProperty::from(-1))
        .unwrap();
    let order: Vec<&str> = graph
        .vertex_properties(b)
        .unwrap()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(order.last(), Some(&"field_7"));
    assert_eq!(order.len(), 40);
    assert_eq!(
        graph.vertex_properties(b).unwrap().unwrap().get("field_8"),
        Some(&ElementProperty::from(8))
    );
}
