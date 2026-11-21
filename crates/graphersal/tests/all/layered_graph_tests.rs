//! `LayeredGraph` (`graphersal::storage::LayeredGraph`).
//!
//! The read path: an untouched layer over a base reads exactly as the base, through the storage
//! API, the typed traversal API, the Rhai DSL and GQL. The write path of new elements (S1.3):
//! vertices and edges between them are stored in the layer and read exactly as in a
//! `TraversalGraph` that received the base's data plus the same writes (ids, auto ids,
//! statistics, scans, label lookups, adjacency); writes to base elements and edges touching base
//! vertices are refused and change nothing.

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{
    Graph, GraphStatistics, GraphStorage, IntoAnyGraph, LayeredEdge, LayeredGraph, LayeredVertex,
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

    // A traversal that writes to a base element fails with the same error.
    let err = GraphTraversalSource::new(&mut layer)
        .v("1")
        .property("age", 1)
        .next()
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("writes to base elements of a layered graph"),
        "{err:?}"
    );
}

#[test]
fn capabilities_claim_only_what_holds_now() {
    let layer = modern_layer();
    let caps = layer.capabilities().unwrap();
    assert!(caps.is_consistent());
    assert!(caps.writable && !caps.atomic && !caps.change_capture);
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
    let actual = render(layered_graph(), r#"g.V("1").property("age", 1).next()"#);
    let err = actual.unwrap_err();
    assert!(
        err.contains("writes to base elements of a layered graph"),
        "{err}"
    );
}

/// Documented examples that read what the layer does not report yet: `g.mark` needs a journal
/// (persistence, stage 3; both fail, with different texts), `g.memory_usage()` is the
/// layer's own figure and `g.shrink_to_fit()` reports what it released by that figure (task
/// S1.11).
#[cfg(all(feature = "script", feature = "io"))]
const PENDING_EXAMPLES: &[(&str, &str)] = &[
    ("traversal", "mark"),
    ("traversal", "memory_usage"),
    ("traversal", "shrink_to_fit"),
];

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

// ------------------------------------------------------------------------------------------
// New elements (S1.3)
// ------------------------------------------------------------------------------------------

/// The properties of a map, in order.
fn entries(map: Option<&PropertyMap>) -> Vec<(String, ElementProperty)> {
    map.map(|map| {
        map.iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    })
    .unwrap_or_default()
}

/// Everything a storage answers about its elements, by id: what a layer must answer exactly as
/// a `TraversalGraph` holding the same data.
#[derive(Debug, PartialEq)]
struct State {
    counts: (usize, usize),
    /// id, labels, properties, out-edge ids, in-edge ids; in scan order.
    vertices: Vec<(
        String,
        Vec<String>,
        Vec<(String, ElementProperty)>,
        Vec<String>,
        Vec<String>,
    )>,
    /// id, label, source id, target id, properties; in scan order.
    edges: Vec<(
        String,
        Option<String>,
        String,
        String,
        Vec<(String, ElementProperty)>,
    )>,
    /// Per label (of every element): the ids of `vertices_by_label` / `edges_by_label`.
    by_label: Vec<(String, Vec<String>, Vec<String>)>,
    statistics: GraphStatistics,
}

fn state<S: GraphStorage>(s: &S) -> State {
    let vid = |n: S::VertexId| s.vertex_id(n).unwrap().unwrap().to_string();
    let eid = |e: S::EdgeId| s.edge_id(e).unwrap().unwrap().to_string();
    let mut labels: Vec<String> = Vec::new();
    let vertices = s
        .vertices()
        .unwrap()
        .map(|n| {
            let own: Vec<String> = s
                .vertex_labels(n)
                .unwrap()
                .iter()
                .map(|l| l.to_string())
                .collect();
            labels.extend(own.iter().cloned());
            // Lookups by id and the liveness check agree with the scan.
            assert_eq!(s.get_vertex_by_id(&vid(n)).unwrap(), Some(n));
            assert!(s.contains_vertex(n).unwrap());
            (
                vid(n),
                own,
                entries(s.vertex_properties(n).unwrap()),
                s.out_edges(n).unwrap().map(eid).collect(),
                s.in_edges(n).unwrap().map(eid).collect(),
            )
        })
        .collect();
    let edges = s
        .edges()
        .unwrap()
        .map(|e| {
            let label = s.edge_label(e).unwrap().map(str::to_string);
            labels.extend(label.iter().cloned());
            assert_eq!(s.get_edge_by_id(&eid(e)).unwrap(), Some(e));
            assert!(s.contains_edge(e).unwrap());
            let (from, to) = s.edge_endpoints(e).unwrap().unwrap();
            (
                eid(e),
                label,
                vid(from),
                vid(to),
                entries(s.edge_properties(e).unwrap()),
            )
        })
        .collect();
    labels.sort();
    labels.dedup();
    let by_label = labels
        .into_iter()
        .map(|label| {
            let vs = s.vertices_by_label(&label).unwrap().map(vid).collect();
            let es = s.edges_by_label(&label).unwrap().map(eid).collect();
            (label, vs, es)
        })
        .collect();
    let statistics = s.statistics().unwrap().unwrap().clone();
    // The incrementally kept statistics equal a full recount.
    assert_eq!(statistics, GraphStatistics::from_storage(s).unwrap());
    State {
        counts: (s.vertex_count().unwrap(), s.edge_count().unwrap()),
        vertices,
        edges,
        by_label,
        statistics,
    }
}

fn props(pairs: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (k, v) in pairs {
        map.insert(CompactString::from(*k), v.clone());
    }
    map
}

/// The same creates on any storage: vertices with explicit and auto ids, several labels, no
/// label; edges between them (labelled, unlabeled, a self-loop) with explicit and auto ids.
fn create_sample<S: GraphStorage>(s: &mut S) -> Vec<String> {
    let a = s
        .insert_vertex(
            Some("a".into()),
            vec!["person".into(), "employee".into(), "person".into()],
            props(&[("name", "ada".into()), ("age", 36.into())]),
        )
        .unwrap();
    let b = s
        .add_vertex(
            None,
            vec!["person".into()],
            props(&[("name", "bob".into())]),
        )
        .unwrap();
    let c = s
        .insert_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    let d = s
        .insert_vertex(
            Some("100".into()),
            vec!["robot".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let e = s
        .insert_vertex(None, vec!["robot".into()], props(&[("n", 1.5.into())]))
        .unwrap();
    s.insert_edge(
        None,
        Some("knows".into()),
        a,
        b,
        props(&[("w", 0.5.into())]),
    )
    .unwrap();
    s.insert_edge(
        Some("k2".into()),
        Some("knows".into()),
        b,
        a,
        PropertyMap::default(),
    )
    .unwrap();
    s.add_edge(None, None, c, d, PropertyMap::default())
        .unwrap();
    s.insert_edge(
        Some("500".into()),
        Some("loop".into()),
        e,
        e,
        PropertyMap::default(),
    )
    .unwrap();
    s.insert_edge(None, Some("built".into()), a, e, PropertyMap::default())
        .unwrap();
    [a, b, c, d, e]
        .into_iter()
        .map(|n| s.vertex_id(n).unwrap().unwrap().to_string())
        .collect()
}

#[test]
fn inserts_on_an_empty_base_read_like_a_traversal_graph() {
    let mut reference = TraversalGraph::new();
    let mut layer = LayeredGraph::default();
    let expected_ids = create_sample(&mut reference);
    let ids = create_sample(&mut layer);
    assert_eq!(ids, expected_ids);
    assert_eq!(ids, ["a", "1", "2", "100", "101"]);
    assert_eq!(state(&layer), state(&reference));
    assert_eq!(layer.vertex_count().unwrap(), 5);
    assert_eq!(layer.edge_count().unwrap(), 5);
    // The new handles are the layer's own (tagged), and a self-loop is in both lists once.
    let robot = v(&layer, "101");
    assert!(format!("{robot:?}").starts_with("v[+"), "{robot:?}");
    assert_eq!(layer.out_edges(robot).unwrap().count(), 1);
    assert_eq!(layer.in_edges(robot).unwrap().count(), 2);
}

#[test]
fn inserts_over_modern_continue_the_base() {
    let base = modern_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut reference = TraversalGraph::tinkerpop_modern();
    let expected_ids = create_sample(&mut reference);
    let ids = create_sample(&mut layer);
    assert_eq!(ids, expected_ids);
    assert_eq!(state(&layer), state(&reference));
    assert_eq!(layer.vertex_count().unwrap(), 11);
    assert_eq!(layer.edge_count().unwrap(), 11);
    // The base is unchanged and still shared.
    assert_eq!(TraversalGraph::vertex_count(&base), 6);
    assert_eq!(TraversalGraph::edge_count(&base), 6);
    assert_eq!(
        GraphStorage::statistics(&*base).unwrap(),
        GraphStorage::statistics(&TraversalGraph::tinkerpop_modern()).unwrap()
    );
    // Base and added elements under one label: base members first, in the base's order.
    let people: Vec<&str> = layer
        .vertices_by_label("person")
        .unwrap()
        .map(|n| layer.vertex_id(n).unwrap().unwrap())
        .collect();
    assert_eq!(people, ["1", "2", "4", "6", "a", ids[1].as_str()]);
    // A second layer over the same base sees none of it.
    let other = LayeredGraph::over(base).unwrap();
    assert_eq!(other.vertex_count().unwrap(), 6);
    assert_eq!(other.get_vertex_by_id("a").unwrap(), None);
}

#[test]
fn ids_are_unique_across_base_and_delta() {
    let mut layer = modern_layer();
    let mut reference = TraversalGraph::tinkerpop_modern();
    // A base vertex id, then an added one.
    fn vertex_err<S: GraphStorage>(s: &mut S, id: &str) -> String {
        format!(
            "{:?}",
            s.insert_vertex(Some(id.into()), vec!["x".into()], PropertyMap::default())
                .map(drop)
                .unwrap_err()
        )
    }
    for s in [vertex_err(&mut layer, "1"), vertex_err(&mut reference, "1")] {
        assert!(s.contains("VertexIdUniqueConstraint"), "{s}");
    }
    assert_eq!(vertex_err(&mut layer, "1"), vertex_err(&mut reference, "1"));
    layer
        .insert_vertex(Some("new".into()), vec![], PropertyMap::default())
        .unwrap();
    reference
        .insert_vertex(Some("new".into()), vec![], PropertyMap::default())
        .unwrap();
    assert_eq!(
        vertex_err(&mut layer, "new"),
        vertex_err(&mut reference, "new")
    );
    // A refused numeric id does not move the sequence: the next auto id is the reference's.
    assert_eq!(vertex_err(&mut layer, "5"), vertex_err(&mut reference, "5"));
    let next = layer
        .insert_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    let expected = reference
        .insert_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    assert_eq!(
        layer.vertex_id(next).unwrap(),
        GraphStorage::vertex_id(&reference, expected).unwrap()
    );
    assert_eq!(layer.vertex_id(next).unwrap(), Some("7"));

    // Edges: a base edge id, an added edge id, then the auto ids continue the base's.
    let (x, y) = (v(&layer, "new"), v(&layer, "7"));
    let (rx, ry) = (
        GraphStorage::get_vertex_by_id(&reference, "new")
            .unwrap()
            .unwrap(),
        GraphStorage::get_vertex_by_id(&reference, "7")
            .unwrap()
            .unwrap(),
    );
    for id in ["0", "5"] {
        let on_layer = layer
            .insert_edge(Some(id.into()), None, x, y, PropertyMap::default())
            .unwrap_err();
        let on_reference = reference
            .insert_edge(Some(id.into()), None, rx, ry, PropertyMap::default())
            .unwrap_err();
        assert!(matches!(
            on_layer,
            GraphError::EdgeIdUniqueConstraint { .. }
        ));
        assert_eq!(format!("{on_layer:?}"), format!("{on_reference:?}"));
    }
    layer
        .insert_edge(Some("e".into()), None, x, y, PropertyMap::default())
        .unwrap();
    reference
        .insert_edge(Some("e".into()), None, rx, ry, PropertyMap::default())
        .unwrap();
    assert!(matches!(
        layer.insert_edge(Some("e".into()), None, x, y, PropertyMap::default()),
        Err(GraphError::EdgeIdUniqueConstraint { .. })
    ));
    for _ in 0..2 {
        let on_layer = layer
            .insert_edge(None, None, x, y, PropertyMap::default())
            .unwrap();
        let on_reference = reference
            .insert_edge(None, None, rx, ry, PropertyMap::default())
            .unwrap();
        assert_eq!(
            layer.edge_id(on_layer).unwrap(),
            GraphStorage::edge_id(&reference, on_reference).unwrap()
        );
    }
    assert_eq!(state(&layer), state(&reference));
}

/// A base whose sequence is behind its numeric ids: auto ids skip every id the base holds.
#[test]
fn auto_ids_skip_ids_the_base_holds() {
    let mut base = TraversalGraph::new();
    for id in ["2", "3"] {
        base.insert_vertex(Some(id.into()), vec![], PropertyMap::default())
            .unwrap();
    }
    let a = GraphStorage::get_vertex_by_id(&base, "2").unwrap().unwrap();
    base.insert_edge(Some("1".into()), None, a, a, PropertyMap::default())
        .unwrap();
    let base = base.with_sequences(0, 0);
    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    let ids: Vec<String> = (0..3)
        .map(|_| {
            let n = layer
                .insert_vertex(None, vec![], PropertyMap::default())
                .unwrap();
            layer.vertex_id(n).unwrap().unwrap().to_string()
        })
        .collect();
    assert_eq!(ids, ["1", "4", "5"]);
    let (x, y) = (v(&layer, "1"), v(&layer, "4"));
    let e = layer
        .insert_edge(None, None, x, y, PropertyMap::default())
        .unwrap();
    assert_eq!(layer.edge_id(e).unwrap(), Some("2"));
    // `raise_id_sequences` moves the layer's sequences only up.
    layer.raise_id_sequences(50, 0).unwrap();
    let n = layer
        .insert_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    assert_eq!(layer.vertex_id(n).unwrap(), Some("51"));
    let e = layer
        .insert_edge(None, None, x, y, PropertyMap::default())
        .unwrap();
    assert_eq!(layer.edge_id(e).unwrap(), Some("3"));
}

#[test]
fn edges_touching_base_vertices_are_refused_for_now() {
    let mut layer = modern_layer();
    let marko = v(&layer, "1");
    let new = layer
        .insert_vertex(None, vec!["person".into()], PropertyMap::default())
        .unwrap();
    let before = state(&layer);
    for (from, to) in [(new, marko), (marko, new), (marko, marko)] {
        let err = layer
            .insert_edge(None, Some("knows".into()), from, to, PropertyMap::default())
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("edges touching base vertices of a layered graph"),
            "{err}"
        );
        assert!(
            layer
                .add_edge(None, Some("knows".into()), from, to, PropertyMap::default())
                .is_err()
        );
    }
    assert_eq!(state(&layer), before);
    // A stale endpoint fails as in a `TraversalGraph`, before the refusal.
    layer.drop_vertex(new).unwrap();
    let err = layer
        .insert_edge(None, None, new, marko, PropertyMap::default())
        .unwrap_err();
    let mut reference = TraversalGraph::tinkerpop_modern();
    let gone = reference
        .insert_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    reference.drop_vertex(gone).unwrap();
    let marko_ref = GraphStorage::get_vertex_by_id(&reference, "1")
        .unwrap()
        .unwrap();
    let expected = reference
        .insert_edge(None, None, gone, marko_ref, PropertyMap::default())
        .unwrap_err();
    assert_eq!(format!("{err:?}"), format!("{expected:?}"));
}

/// Changes and drops of added elements, step by step on a layer and on a `TraversalGraph`.
#[test]
fn added_elements_change_and_drop_like_a_traversal_graph() {
    fn run<S: GraphStorage>(s: &mut S) -> Vec<State> {
        let mut states = Vec::new();
        let ids = create_sample(s);
        let n = |s: &S, i: usize| s.get_vertex_by_id(&ids[i]).unwrap().unwrap();
        let (a, b, e) = (n(s, 0), n(s, 1), n(s, 4));
        let knows = s
            .out_edges(a)
            .unwrap()
            .find(|x| s.edge_label(*x).unwrap() == Some("knows"))
            .unwrap();
        s.set_vertex_property(a, "age".into(), 37.into()).unwrap();
        s.set_vertex_property(a, "city".into(), "Rome".into())
            .unwrap();
        s.drop_vertex_property("name", a).unwrap();
        s.drop_vertex_property("absent", a).unwrap();
        s.set_edge_property(knows, "since".into(), 2020.into())
            .unwrap();
        s.drop_edge_property("w", knows).unwrap();
        states.push(state(s));
        assert!(s.add_vertex_label(b, "manager".into()).unwrap());
        assert!(!s.add_vertex_label(b, "manager".into()).unwrap());
        assert!(s.remove_vertex_label(a, "person").unwrap());
        assert!(!s.remove_vertex_label(a, "person").unwrap());
        assert!(s.set_edge_label(knows, "likes".into()).unwrap());
        assert!(!s.set_edge_label(knows, "likes".into()).unwrap());
        states.push(state(s));
        let path: JsonPath = "meta.city".parse().unwrap();
        s.set_vertex_property_path(b, &path, "x".into()).unwrap();
        let tags: JsonPath = "meta.tags".parse().unwrap();
        s.set_vertex_property_path(b, &tags, ElementProperty::Array(vec!["a".into()]))
            .unwrap();
        let append: JsonPath = "meta.tags[1]".parse().unwrap();
        s.set_vertex_property_path(b, &append, "y".into()).unwrap();
        s.set_edge_property_path(knows, &path, 1.into()).unwrap();
        s.remove_vertex_property_path(b, &path).unwrap();
        s.remove_vertex_property_path(b, &"meta.tags[0]".parse().unwrap())
            .unwrap();
        s.remove_edge_property_path(knows, &"since".parse().unwrap())
            .unwrap();
        states.push(state(s));
        // Drop an edge, then a vertex with its edges (a self-loop among them).
        s.drop_edge(knows).unwrap();
        s.drop_edge(knows).unwrap();
        s.drop_vertex(e).unwrap();
        s.drop_vertex(e).unwrap();
        states.push(state(s));
        // The freed slots are reused; the stale handles never reach the new occupants.
        let f = s
            .insert_vertex(
                Some("f".into()),
                vec!["robot".into()],
                PropertyMap::default(),
            )
            .unwrap();
        let g = s
            .insert_vertex(None, vec![], PropertyMap::default())
            .unwrap();
        let fg = s
            .insert_edge(None, Some("next".into()), f, g, PropertyMap::default())
            .unwrap();
        assert!(!s.contains_vertex(e).unwrap());
        assert_eq!(s.vertex_id(e).unwrap(), None);
        assert!(s.vertex_properties(e).unwrap().is_none());
        assert_eq!(s.out_edges(e).unwrap().count(), 0);
        assert!(!s.contains_edge(knows).unwrap());
        assert_eq!(s.edge_endpoints(knows).unwrap(), None);
        assert!(s.set_vertex_property(e, "k".into(), 1.into()).is_err());
        assert!(
            s.insert_edge(None, None, e, f, PropertyMap::default())
                .is_err()
        );
        assert!(s.contains_edge(fg).unwrap());
        states.push(state(s));
        states
    }
    let on_layer = run(&mut modern_layer());
    let on_reference = run(&mut TraversalGraph::tinkerpop_modern());
    assert_eq!(on_layer, on_reference);
    let on_layer = run(&mut LayeredGraph::default());
    let on_reference = run(&mut TraversalGraph::new());
    assert_eq!(on_layer, on_reference);
}

/// The base's schema applies to the layer's creates: coercion, defaults, violations (same
/// errors as a `TraversalGraph` with that schema), closed-mode topology.
#[test]
fn the_base_schema_applies_to_creates() {
    const SCHEMA: &str = r#"{
      "mode": "closed",
      "vertices": {
        "person": {"schema": {"type": "object", "properties": {
          "name": {"type": "string"},
          "score": {"type": "number"},
          "level": {"type": "integer", "default": 1}
        }, "required": ["name"]}},
        "software": {"schema": {"type": "object", "properties": {"name": {"type": "string"}}}}
      },
      "edges": {
        "knows": {"connections": [{"from": "person", "to": "person"}]}
      }
    }"#;
    fn graph() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        graph
            .set_schema(GraphSchema::from_json(SCHEMA).unwrap())
            .unwrap();
        graph
    }
    fn run<S: GraphStorage>(s: &mut S) -> (Vec<String>, State) {
        let mut outcomes = Vec::new();
        let mut record = |r: Result<String, GraphError>| {
            outcomes.push(match r {
                Ok(v) => v,
                Err(e) => format!("{e:?}"),
            })
        };
        let p = s
            .add_vertex(
                None,
                vec!["person".into()],
                props(&[("name", "p".into()), ("score", 3.into())]),
            )
            .unwrap();
        let q = s
            .insert_vertex(None, vec!["person".into()], props(&[("name", "q".into())]))
            .unwrap();
        let w = s
            .insert_vertex(
                None,
                vec!["software".into()],
                props(&[("name", "w".into())]),
            )
            .unwrap();
        record(
            s.insert_vertex(None, vec!["robot".into()], PropertyMap::default())
                .map(|_| "ok".into()),
        );
        record(
            s.insert_vertex(None, vec!["person".into()], PropertyMap::default())
                .map(|_| "ok".into()),
        );
        record(
            s.insert_vertex(None, vec!["person".into()], props(&[("name", 1.into())]))
                .map(|_| "ok".into()),
        );
        record(
            s.insert_edge(None, Some("knows".into()), p, q, PropertyMap::default())
                .map(|_| "ok".into()),
        );
        record(
            s.insert_edge(None, Some("knows".into()), p, w, PropertyMap::default())
                .map(|_| "ok".into()),
        );
        record(
            s.insert_edge(None, Some("uses".into()), p, w, PropertyMap::default())
                .map(|_| "ok".into()),
        );
        record(
            s.set_vertex_property(q, "score".into(), 2.into())
                .map(|_| "ok".into()),
        );
        record(
            s.set_vertex_property(q, "other".into(), 2.into())
                .map(|_| "ok".into()),
        );
        record(s.drop_vertex_property("name", q).map(|_| "ok".into()));
        record(
            s.add_vertex_label(w, "person".into())
                .map(|b| b.to_string()),
        );
        (outcomes, state(s))
    }
    let on_layer = run(&mut LayeredGraph::over(Arc::new(graph())).unwrap());
    let on_reference = run(&mut graph());
    assert_eq!(on_layer, on_reference);
    // Coerced and defaulted as stored: `score` became a number, `level` came from the default.
    assert!(
        on_layer.0.iter().filter(|o| *o == "ok").count() >= 2,
        "{:?}",
        on_layer.0
    );
    let p = &on_layer.1.vertices[0];
    assert!(p.2.iter().any(|(k, _)| k == "level"), "{p:?}");
}

#[test]
fn typed_traversals_create_and_read_back() {
    let mut layer = modern_layer();
    let mut reference = TraversalGraph::tinkerpop_modern();
    macro_rules! both {
        ($($q:tt)*) => {{
            let on_reference = GraphTraversalSource::new(&mut reference).$($q)*.to_list();
            let on_layer = GraphTraversalSource::new(&mut layer).$($q)*.to_list();
            assert_eq!(
                format!("{on_layer:?}"),
                format!("{on_reference:?}"),
                stringify!($($q)*)
            );
        }};
    }
    both!(
        add_v("person")
            .property("name", "ada")
            .property("age", 36)
            .id()
    );
    both!(add_v("robot").property("name", "r2").id());
    both!(v("7").add_e("knows").to("8").property("w", 1).label());
    both!(v(None).has_label("person").values("name"));
    both!(v("7").out("knows").values("name"));
    both!(v("8").in_("knows").id());
    both!(e(None).has_label("knows").count());
    both!(v("7").property("age", 40).values("age"));
    both!(v("8").drop());
    both!(v(None).count());
    assert_eq!(state(&layer), state(&reference));
}

/// Scripts that create new elements (and change or drop only what they created): the same
/// result and the same graph state afterwards on the plain graph and on the layer over it.
#[cfg(feature = "script")]
const RHAI_CREATES: &[&str] = &[
    r#"g.addV("person").property("name", "ann").property("age", 41).values("name").next()"#,
    r#"g.add_v("person").property("name", "ada").values("name").next()"#,
    r#"g.addV("person").property(T.id, "x1").property("name", "x").id().next()"#,
    r#"let a = g.addV("person").property("name", "a").id().next(); let b = g.addV("person").property("name", "b").id().next(); g.V(a).addE("knows").to(__.V(b)).property("w", 0.5).iterate(); g.V(a).out("knows").values("name").toList()"#,
    r#"g.addV("p").as("a").addV("p").as("b").addE("e").from("a").to("b").iterate(); [g.V().count().next(), g.E().count().next(), g.E().hasLabel("e").count().next()]"#,
    r#"let a = g.addV("n").id().next(); g.V(a).addE("self").to(__.V(a)).iterate(); [g.V(a).out("self").id().toList(), g.V(a).both().count().next()]"#,
    r#"g.inject(1, 2, 3).as("x").addV("num").property("v", __.select("x")).iterate(); g.V().hasLabel("num").values("v").order().toList()"#,
    r#"g.addV("s").property(jpath("meta.tags"), ["a"]).property(jpath("meta.tags[1]"), "b").values(jpath("meta.tags")).next()"#,
    r#"let a = g.addV("t").property("k", 1).id().next(); g.V(a).property("k", 2).iterate(); g.V(a).properties("k").drop().iterate(); g.V(a).valueMap().next()"#,
    r#"let a = g.addV("t").id().next(); g.V(a).add_label("u").iterate(); g.V(a).labels().next()"#,
    r#"let a = g.addV("t").id().next(); let b = g.addV("t").id().next(); g.V(a).addE("x").to(__.V(b)).iterate(); g.V(a).drop().iterate(); [g.V().count().next(), g.E().count().next()]"#,
    r#"g.mergeV(#{"T.id": "9", "T.label": "person", name: "zed"}).values("name").next()"#,
    r#"g.mergeV(#{"T.label": "person", name: "marko"}).values("age").next()"#,
    r#"let a = g.addV("t").id().next(); let b = g.addV("t").id().next(); g.mergeE(#{"T.label": "r", "Direction.OUT": a, "Direction.IN": b}).iterate(); g.E().hasLabel("r").count().next()"#,
    r#"g.addV("person").property("name", "z").iterate(); g.V().hasLabel("person").order().by("name").values("name").toList()"#,
    r#"g.addV("person").iterate(); g.statistics()"#,
];

#[cfg(feature = "script")]
const STATE_SCRIPT: &str = r#"[
    g.V().elementMap().toList(),
    g.E().elementMap().toList(),
    g.V().group().by(T.label).by(__.count()).next(),
    g.statistics()
]"#;

#[cfg(feature = "script")]
#[test]
fn rhai_create_scripts_match_the_plain_graph() {
    let mut differences = Vec::new();
    for script in RHAI_CREATES {
        let plain = plain_graph();
        let layered = layered_graph();
        let expected = render(Arc::clone(&plain), script);
        let actual = render(Arc::clone(&layered), script);
        assert!(expected.is_ok(), "{script}: {expected:?}");
        let expected_state = render(plain, STATE_SCRIPT);
        let actual_state = render(layered, STATE_SCRIPT);
        if expected != actual || expected_state != actual_state {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}\n  state TraversalGraph: {expected_state:?}\n  state LayeredGraph:   {actual_state:?}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// The same scripts on a layer over an empty base against an empty `TraversalGraph`.
#[cfg(feature = "script")]
#[test]
fn rhai_create_scripts_on_an_empty_base_match_an_empty_graph() {
    let mut differences = Vec::new();
    for script in RHAI_CREATES {
        let plain = Arc::new(Graph::new(TraversalGraph::new()));
        let layered = Arc::new(Graph::new(LayeredGraph::default()));
        let expected = render(Arc::clone(&plain), script);
        let actual = render(Arc::clone(&layered), script);
        let expected_state = render(plain, STATE_SCRIPT);
        let actual_state = render(layered, STATE_SCRIPT);
        if expected != actual || expected_state != actual_state {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}\n  state TraversalGraph: {expected_state:?}\n  state LayeredGraph:   {actual_state:?}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
