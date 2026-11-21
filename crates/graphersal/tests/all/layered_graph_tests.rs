//! `LayeredGraph` (`graphersal::storage::LayeredGraph`), its read path: an untouched layer
//! over a base reads exactly as the base, through the storage API, the typed traversal API, the
//! Rhai DSL and GQL; every write is refused and changes nothing.

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{
    Graph, GraphStorage, IntoAnyGraph, LayeredEdge, LayeredGraph, LayeredVertex,
};

fn modern_base() -> Arc<TraversalGraph> {
    Arc::new(TraversalGraph::tinkerpop_modern())
}

fn modern_layer() -> LayeredGraph {
    LayeredGraph::over(modern_base()).unwrap()
}

/// The vertex of `layer` with `id`.
fn v(layer: &LayeredGraph, id: &str) -> LayeredVertex {
    layer.get_vertex_by_id(id).unwrap().unwrap()
}

#[test]
fn an_untouched_layer_reads_like_its_base() {
    let base = modern_base();
    let layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let base = &*base;

    assert_eq!(layer.vertex_count().unwrap(), 6);
    assert_eq!(layer.edge_count().unwrap(), 6);

    // Scans in the base's order, with the same ids.
    let base_vertices: Vec<&str> = GraphStorage::vertices(base)
        .unwrap()
        .map(|n| GraphStorage::vertex_id(base, n).unwrap().unwrap())
        .collect();
    let layer_vertices: Vec<&str> = layer
        .vertices()
        .unwrap()
        .map(|n| layer.vertex_id(n).unwrap().unwrap())
        .collect();
    assert_eq!(layer_vertices, base_vertices);
    let base_edges: Vec<&str> = GraphStorage::edges(base)
        .unwrap()
        .map(|x| GraphStorage::edge_id(base, x).unwrap().unwrap())
        .collect();
    let layer_edges: Vec<&str> = layer
        .edges()
        .unwrap()
        .map(|x| layer.edge_id(x).unwrap().unwrap())
        .collect();
    assert_eq!(layer_edges, base_edges);

    // Every element read, per element.
    for node in layer.vertices().unwrap() {
        let id = layer.vertex_id(node).unwrap().unwrap();
        let reference = GraphStorage::get_vertex_by_id(base, id).unwrap().unwrap();
        assert_eq!(layer.get_vertex_by_id(id).unwrap(), Some(node));
        assert_eq!(
            layer.vertex_label(node).unwrap(),
            GraphStorage::vertex_label(base, reference).unwrap()
        );
        assert_eq!(
            layer.vertex_labels(node).unwrap(),
            GraphStorage::vertex_labels(base, reference).unwrap()
        );
        assert!(
            layer
                .vertex_has_label(node, layer.vertex_label(node).unwrap().unwrap())
                .unwrap()
        );
        assert!(!layer.vertex_has_label(node, "nothing").unwrap());
        assert_eq!(
            layer.vertex_properties(node).unwrap(),
            GraphStorage::vertex_properties(base, reference).unwrap()
        );
        assert!(layer.contains_vertex(node).unwrap());
        let edge_ids = |edges: Vec<LayeredEdge>| -> Vec<&str> {
            edges
                .into_iter()
                .map(|x| layer.edge_id(x).unwrap().unwrap())
                .collect()
        };
        let base_edge_ids = |edges: Vec<graphersal::storage::EdgeHandle>| -> Vec<&str> {
            edges
                .into_iter()
                .map(|x| GraphStorage::edge_id(base, x).unwrap().unwrap())
                .collect()
        };
        assert_eq!(
            edge_ids(layer.out_edges(node).unwrap().collect()),
            base_edge_ids(GraphStorage::out_edges(base, reference).unwrap().collect())
        );
        assert_eq!(
            edge_ids(layer.in_edges(node).unwrap().collect()),
            base_edge_ids(GraphStorage::in_edges(base, reference).unwrap().collect())
        );
    }
    for edge in layer.edges().unwrap() {
        let id = layer.edge_id(edge).unwrap().unwrap();
        let reference = GraphStorage::get_edge_by_id(base, id).unwrap().unwrap();
        assert_eq!(layer.get_edge_by_id(id).unwrap(), Some(edge));
        assert_eq!(
            layer.edge_label(edge).unwrap(),
            GraphStorage::edge_label(base, reference).unwrap()
        );
        assert_eq!(
            layer.edge_properties(edge).unwrap(),
            GraphStorage::edge_properties(base, reference).unwrap()
        );
        let (from, to) = layer.edge_endpoints(edge).unwrap().unwrap();
        let (base_from, base_to) = GraphStorage::edge_endpoints(base, reference)
            .unwrap()
            .unwrap();
        assert_eq!(
            layer.vertex_id(from).unwrap(),
            GraphStorage::vertex_id(base, base_from).unwrap()
        );
        assert_eq!(
            layer.vertex_id(to).unwrap(),
            GraphStorage::vertex_id(base, base_to).unwrap()
        );
        assert!(layer.out_edges(from).unwrap().any(|x| x == edge));
        assert!(layer.in_edges(to).unwrap().any(|x| x == edge));
        assert!(layer.contains_edge(edge).unwrap());
    }

    // Label lookups through the base's label index, missing ids and labels.
    let people: Vec<&str> = layer
        .vertices_by_label("person")
        .unwrap()
        .map(|n| layer.vertex_id(n).unwrap().unwrap())
        .collect();
    assert_eq!(people, ["1", "2", "4", "6"]);
    assert_eq!(layer.edges_by_label("created").unwrap().count(), 4);
    assert_eq!(layer.vertices_by_label("nothing").unwrap().count(), 0);
    assert_eq!(layer.edges_by_label("nothing").unwrap().count(), 0);
    assert_eq!(layer.get_vertex_by_id("nothing").unwrap(), None);
    assert_eq!(layer.get_edge_by_id("nothing").unwrap(), None);

    // Statistics, schema and catalog are the base's.
    assert_eq!(
        layer.statistics().unwrap(),
        GraphStorage::statistics(base).unwrap()
    );
    assert_eq!(
        layer.get_schema().unwrap(),
        GraphStorage::get_schema(base).unwrap()
    );
    assert_eq!(layer.definitions().unwrap().count(), 0);
}

#[test]
fn many_layers_share_one_base() {
    let base = modern_base();
    let layers: Vec<LayeredGraph> = (0..3)
        .map(|_| LayeredGraph::over(Arc::clone(&base)).unwrap())
        .collect();
    assert_eq!(Arc::strong_count(&base), 4);
    assert!(layers.iter().all(|layer| Arc::ptr_eq(layer.base(), &base)));
    // Handles of different layers over one base name the same base slots.
    assert_eq!(v(&layers[0], "1"), v(&layers[1], "1"));
    // Layers read the shared base from several threads at once.
    std::thread::scope(|scope| {
        for layer in &layers {
            scope.spawn(move || {
                let g = GraphTraversalSource::new(layer);
                assert_eq!(
                    g.v(None).out(None).count().next().unwrap(),
                    Some(6_i64.into())
                );
            });
        }
    });
}

#[test]
fn a_default_layer_is_over_an_empty_base() {
    let layer = LayeredGraph::default();
    assert_eq!(layer.vertex_count().unwrap(), 0);
    assert_eq!(layer.edge_count().unwrap(), 0);
    assert_eq!(layer.vertices().unwrap().count(), 0);
    assert_eq!(GraphStorage::vertex_count(&**layer.base()).unwrap(), 0);
    let graph = Graph::new(LayeredGraph::default());
    assert_eq!(graph.read().vertex_count().unwrap(), 0);
}

#[test]
fn every_write_is_refused_and_changes_nothing() {
    let mut layer = modern_layer();
    let marko = v(&layer, "1");
    let knows = layer.out_edges(marko).unwrap().next().unwrap();
    let unsupported = |result: Result<(), GraphError>| {
        assert!(
            matches!(result, Err(GraphError::Unsupported { .. })),
            "{result:?}"
        );
    };
    unsupported(
        layer
            .insert_vertex(None, vec!["person".into()], PropertyMap::default())
            .map(drop),
    );
    unsupported(
        layer
            .add_vertex(None, vec!["person".into()], PropertyMap::default())
            .map(drop),
    );
    unsupported(
        layer
            .insert_edge(
                None,
                Some("knows".into()),
                marko,
                marko,
                PropertyMap::default(),
            )
            .map(drop),
    );
    unsupported(layer.add_vertex_label(marko, "x".into()).map(drop));
    unsupported(layer.remove_vertex_label(marko, "person").map(drop));
    unsupported(layer.set_edge_label(knows, "x".into()).map(drop));
    unsupported(layer.set_vertex_property(marko, "age".into(), 1.into()));
    unsupported(layer.set_edge_property(knows, "weight".into(), 1.into()));
    unsupported(layer.drop_vertex_property("age", marko));
    unsupported(layer.drop_edge_property("weight", knows));
    unsupported(layer.drop_edge(knows));
    unsupported(layer.drop_vertex(marko));
    unsupported(layer.set_schema(graphersal::schema::GraphSchema::new()));
    assert_eq!(layer.vertex_count().unwrap(), 6);
    assert_eq!(layer.edge_count().unwrap(), 6);
    assert_eq!(
        layer.vertex_properties(marko).unwrap().unwrap().get("age"),
        Some(&29.into())
    );
    assert!(layer.contains_edge(knows).unwrap());

    // A traversal that writes fails with the same error.
    let err = GraphTraversalSource::new(&mut layer)
        .add_v("person")
        .next()
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("writes to a layered graph"),
        "{err:?}"
    );
}

#[test]
fn capabilities_claim_only_what_holds_now() {
    let layer = modern_layer();
    let caps = layer.capabilities().unwrap();
    assert!(caps.is_consistent());
    assert!(!caps.writable && !caps.atomic && !caps.change_capture);
    assert!(!caps.immutable && !caps.durability && !caps.property_indexes);
    assert_eq!(
        caps.exact_property_index_lookups,
        GraphStorage::capabilities(&TraversalGraph::new())
            .unwrap()
            .exact_property_index_lookups
    );
}

#[test]
fn typed_traversals_match_the_base() {
    let base = modern_base();
    let layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    macro_rules! same {
        ($($q:tt)*) => {{
            let on_base = GraphTraversalSource::new(&*base).$($q)*.to_list().unwrap();
            let on_layer = GraphTraversalSource::new(&layer).$($q)*.to_list().unwrap();
            assert_eq!(on_layer, on_base, stringify!($($q)*));
        }};
    }
    same!(v(None).values("name"));
    same!(v(None).has_label("person").values("age"));
    same!(v("1").out("knows").values("name"));
    same!(v(None).out(None).out(None).values("name"));
    same!(v("3").in_("created").values("name"));
    same!(e(None).has_label("knows").label());
    same!(v(None).out_e(None).count());
    same!(v(None).both(None).dedup().count());
    same!(v(None).id());
    same!(e(None).id());
}

/// `(script).to_string()` on `graph`, or the error text.
#[cfg(feature = "script")]
fn render(graph: impl IntoAnyGraph, script: &str) -> Result<String, String> {
    crate::script_host::eval_value_with_limits(
        graph,
        &format!("({{ {script} }}).to_string()"),
        HashMap::new(),
        &crate::script_host::ScriptLimits::default(),
    )
    .map(|value| value.to_string())
    .map_err(|err| err.to_string())
}

#[cfg(feature = "script")]
fn plain_graph() -> Arc<Graph<TraversalGraph>> {
    Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()))
}

#[cfg(feature = "script")]
fn layered_graph() -> Arc<Graph<LayeredGraph>> {
    Arc::new(Graph::new(modern_layer()))
}

/// Read queries of the Rhai DSL: scans, adjacency, filters, projections, paths, aggregates.
#[cfg(feature = "script")]
const RHAI_READS: &[&str] = &[
    r#"g.V().elementMap().toList()"#,
    r#"g.E().elementMap().toList()"#,
    r#"g.V().hasLabel("person").values("name").toList()"#,
    r#"g.V("1").out("knows").values("name").toList()"#,
    r#"g.V().out().out().path().by("name").toList()"#,
    r#"g.V().has("age", gt(30)).valueMap().toList()"#,
    r#"g.V().group().by(T.label).by(__.count()).next()"#,
    r#"g.V().hasLabel("software").in("created").dedup().values("name").fold().next()"#,
    r#"g.E().hasLabel("created").outV().values("name").toList()"#,
    r#"g.V().repeat(__.out()).times(2).emit().values("name").toList()"#,
    r#"g.V().both().both().count().next()"#,
    r#"g.V().order().by("name", Order.desc).limit(3).values("name").toList()"#,
    r#"g.V().project("n", "d").by("name").by(__.bothE().count()).toList()"#,
    r#"g.V("1").outE().inV().id().toList()"#,
    r#"g.V().has("name", "josh").label().next()"#,
    r#"g.statistics()"#,
    r#"g.get_schema()"#,
];

#[cfg(feature = "script")]
#[test]
fn rhai_reads_match_the_plain_graph() {
    let mut differences = Vec::new();
    for script in RHAI_READS {
        let expected = render(plain_graph(), script);
        let actual = render(layered_graph(), script);
        assert!(expected.is_ok(), "{script}: {expected:?}");
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[cfg(feature = "script")]
#[test]
fn rhai_writes_fail_on_the_layer() {
    let actual = render(layered_graph(), r#"g.addV("x").next()"#);
    let err = actual.unwrap_err();
    assert!(err.contains("writes to a layered graph"), "{err}");
}

/// Documented examples that read what the layer does not report yet: `g.mark` needs a journal
/// (persistence, stage 3; both fail, with different texts) and `g.memory_usage()` is the
/// layer's own figure (task S1.11).
#[cfg(all(feature = "script", feature = "io"))]
const PENDING_EXAMPLES: &[(&str, &str)] = &[("traversal", "mark"), ("traversal", "memory_usage")];

/// Every documented DSL example either gives the same result on the layer as on the plain
/// graph, or fails on the layer because it writes. A read that differs is a bug.
#[cfg(all(feature = "script", feature = "io"))]
#[test]
fn every_documented_read_example_matches_the_plain_graph() {
    let mut differences = Vec::new();
    let (mut same, mut writes) = (0, 0);
    for (receiver, name, example, result) in graphersal::script::doc_examples() {
        if result.is_none() || PENDING_EXAMPLES.contains(&(receiver, name)) {
            continue;
        }
        let expected = render(plain_graph(), example);
        let actual = render(layered_graph(), example);
        if expected == actual {
            same += 1;
        } else if actual
            .as_ref()
            .is_err_and(|err| err.contains("does not support"))
        {
            writes += 1;
        } else {
            differences.push(format!(
                "{receiver}.{name}: `{example}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}"
            ));
        }
    }
    assert!(
        same > 150,
        "only {same} examples matched ({writes} refused writes)"
    );
    assert!(
        differences.is_empty(),
        "{} differences ({same} same, {writes} refused writes):\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// GQL reads: node and edge patterns, joins, ordering, aggregates, parameters.
#[cfg(all(feature = "script", feature = "gql"))]
const GQL_READS: &[&str] = &[
    r#"g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name, p.age AS age").toList()"#,
    r#"g.gql("MATCH (n:!person) RETURN n.name, n.lang, labels(n), element_id(n)").toList()"#,
    r#"g.gql("MATCH (a:person)-[e:knows]->(b) RETURN a.name, e.weight, b.name, type(e)").toList()"#,
    r#"g.gql("MATCH (s:software)<-[:created]-(p) RETURN s.name, p.name").toList()"#,
    r#"g.gql("MATCH (a)-[e]-(b) WHERE a.name = 'josh' RETURN b.name, element_id(e)").toList()"#,
    r#"g.gql("MATCH (a)-[:created]->(s)<-[:created]-(b) WHERE a <> b RETURN a.name, b.name, s.name").toList()"#,
    r#"g.gql("MATCH (p) RETURN p.name AS n, p.age AS a ORDER BY a DESC, n OFFSET 1 LIMIT 3").toList()"#,
    r#"g.gql("MATCH (p:person)-[:created]->(s) RETURN s.name AS s, count(*) AS c, sum(p.age) AS total ORDER BY s").toList()"#,
    r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS p, s.name AS s ORDER BY p, s").toList()"#,
    r#"g.gql("MATCH (p {name: $name})-[:knows]->(f) RETURN f.name AS f", #{name: "marko"}).select("f").fold().next()"#,
];

#[cfg(all(feature = "script", feature = "gql"))]
#[test]
fn gql_reads_match_the_plain_graph() {
    let mut differences = Vec::new();
    for script in GQL_READS {
        let expected = render(plain_graph(), script);
        let actual = render(layered_graph(), script);
        assert!(expected.is_ok(), "{script}: {expected:?}");
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
