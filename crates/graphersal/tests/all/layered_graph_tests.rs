//! `LayeredGraph` (`graphersal::storage::LayeredGraph`).
//!
//! The read path: an untouched layer over a base reads exactly as the base, through the storage
//! API, the typed traversal API, the Rhai DSL and GQL. The write path of new elements (S1.3):
//! vertices and edges between them are stored in the layer and read exactly as in a
//! `TraversalGraph` that received the base's data plus the same writes (ids, auto ids,
//! statistics, scans, label lookups, adjacency). New edges touching base vertices (S1.4): every combination of base and added
//! endpoints, self-loops, drops, the closed-schema topology, and a Rhai differential. Drops of
//! base elements (S1.5): cascades, filtered reads, id reuse, stale handles, statistics and a
//! Rhai differential (plan decision P4 for drop-then-add order). Writes to base elements
//! (S1.6): property, path and label writes copy the element up into an image; equal-value writes
//! make none, an image equal to the base again is dropped; schema errors, coded and compressed
//! bases, label lookups, property-index lookups and a Rhai differential.

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::storage::{
    Graph, GraphStatistics, GraphStorage, IntoAnyGraph, JsonPath, LayeredEdge, LayeredGraph,
    LayeredVertex,
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
    let unsupported = |result: Result<(), GraphError>| {
        assert!(
            matches!(result, Err(GraphError::Unsupported { .. })),
            "{result:?}"
        );
    };
    unsupported(layer.set_schema(graphersal::schema::GraphSchema::new()));
    assert_eq!(layer.vertex_count().unwrap(), 6);
    assert_eq!(layer.edge_count().unwrap(), 6);
    assert_eq!(
        layer.vertex_properties(marko).unwrap().unwrap().get("age"),
        Some(&29.into())
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
    let actual = render(layered_graph(), r#"g.set_schema(g.infer_schema())"#);
    let err = actual.unwrap_err();
    assert!(err.contains("does not support"), "{err}");
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

// ------------------------------------------------------------------------------------------
// New edges touching base vertices (S1.4)
// ------------------------------------------------------------------------------------------

/// The vertex with `id` on any storage.
fn vertex_of<S: GraphStorage>(s: &S, id: &str) -> S::VertexId {
    s.get_vertex_by_id(id).unwrap().unwrap()
}

/// The edge ids of `edges` on any storage.
fn edge_ids<S: GraphStorage>(s: &S, edges: &[S::EdgeId]) -> Vec<String> {
    edges
        .iter()
        .map(|e| s.edge_id(*e).unwrap().unwrap().to_string())
        .collect()
}

/// New edges with every combination of base and added endpoints, on any storage holding
/// modern: added -> base, base -> added, base -> base (a base label and a new one), self-loops
/// at a base and at an added vertex, added -> added; explicit, numeric and auto ids; `insert_*`
/// and `add_*`.
fn create_cross_edges<S: GraphStorage>(s: &mut S) -> Vec<S::EdgeId> {
    let (marko, vadas, lop, josh) = (
        vertex_of(s, "1"),
        vertex_of(s, "2"),
        vertex_of(s, "3"),
        vertex_of(s, "4"),
    );
    let n1 = s
        .insert_vertex(
            Some("n1".into()),
            vec!["person".into()],
            props(&[("name", "n1".into())]),
        )
        .unwrap();
    let n2 = s
        .add_vertex(
            None,
            vec!["software".into()],
            props(&[("name", "n2".into())]),
        )
        .unwrap();
    let none = PropertyMap::default;
    vec![
        s.insert_edge(
            None,
            Some("knows".into()),
            n1,
            marko,
            props(&[("w", 1.into())]),
        )
        .unwrap(),
        s.insert_edge(Some("x1".into()), Some("knows".into()), marko, n1, none())
            .unwrap(),
        s.add_edge(None, Some("created".into()), josh, lop, none())
            .unwrap(),
        s.insert_edge(None, Some("likes".into()), marko, vadas, none())
            .unwrap(),
        s.insert_edge(None, Some("self".into()), vadas, vadas, none())
            .unwrap(),
        s.insert_edge(None, Some("self".into()), n2, n2, none())
            .unwrap(),
        s.insert_edge(None, None, n1, n2, none()).unwrap(),
        s.insert_edge(Some("100".into()), Some("created".into()), n1, lop, none())
            .unwrap(),
        s.add_edge(None, Some("created".into()), marko, n2, none())
            .unwrap(),
    ]
}

#[test]
fn edges_touching_base_vertices_read_like_a_traversal_graph() {
    let base = modern_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut reference = TraversalGraph::tinkerpop_modern();
    let on_layer = create_cross_edges(&mut layer);
    let on_reference = create_cross_edges(&mut reference);
    assert_eq!(
        edge_ids(&layer, &on_layer),
        edge_ids(&reference, &on_reference)
    );
    assert_eq!(
        edge_ids(&layer, &on_layer),
        ["6", "x1", "7", "8", "9", "10", "11", "100", "101"]
    );
    // Scans, lookups, both adjacency lists of every vertex (in order), endpoints, labels and
    // the kept statistics against a recount: all as in the `TraversalGraph`.
    assert_eq!(state(&layer), state(&reference));
    // The stubs of the base vertices are not vertices.
    assert_eq!(layer.vertex_count().unwrap(), 8);
    assert_eq!(layer.vertices().unwrap().count(), 8);
    assert_eq!(layer.edge_count().unwrap(), 15);
    assert_eq!(layer.statistics().unwrap().unwrap().vertex_count(), 8);
    assert_eq!(layer.vertices_by_label("person").unwrap().count(), 5);
    // A new edge's base endpoint is the base vertex's own handle.
    let (marko, vadas, n1) = (v(&layer, "1"), v(&layer, "2"), v(&layer, "n1"));
    assert_eq!(
        layer.edge_endpoints(on_layer[0]).unwrap(),
        Some((n1, marko))
    );
    assert_eq!(
        layer.edge_endpoints(on_layer[4]).unwrap(),
        Some((vadas, vadas))
    );
    assert!(!format!("{marko:?}").starts_with("v[+"), "{marko:?}");
    // Adjacency: the new edges first (newest first), then the base's.
    let out: Vec<&str> = layer
        .out_edges(marko)
        .unwrap()
        .map(|e| layer.edge_id(e).unwrap().unwrap())
        .collect();
    assert_eq!(out, ["101", "8", "x1", "2", "1", "0"]);
    // A base self-loop is in both lists, once each.
    assert_eq!(layer.out_edges(vadas).unwrap().count(), 1);
    assert_eq!(layer.in_edges(vadas).unwrap().count(), 3);
    // A base vertex with new edges still reads as the base holds it.
    for id in ["1", "2", "3", "4"] {
        let on_base = GraphStorage::get_vertex_by_id(&*base, id).unwrap().unwrap();
        let node = v(&layer, id);
        assert!(layer.contains_vertex(node).unwrap());
        assert_eq!(layer.vertex_id(node).unwrap(), Some(id));
        assert_eq!(
            layer.vertex_labels(node).unwrap(),
            GraphStorage::vertex_labels(&*base, on_base).unwrap()
        );
        assert_eq!(
            layer.vertex_label(node).unwrap(),
            GraphStorage::vertex_label(&*base, on_base).unwrap()
        );
        assert_eq!(
            layer.vertex_properties(node).unwrap(),
            GraphStorage::vertex_properties(&*base, on_base).unwrap()
        );
    }
    // The base is unchanged; a second layer over it sees none of the new edges.
    assert_eq!(TraversalGraph::edge_count(&base), 6);
    let other = LayeredGraph::over(base).unwrap();
    assert_eq!(other.out_edges(v(&other, "1")).unwrap().count(), 3);
    assert_eq!(other.edge_count().unwrap(), 6);
}

#[test]
fn drops_of_new_edges_at_base_vertices_match_a_traversal_graph() {
    fn run<S: GraphStorage>(s: &mut S) -> Vec<State> {
        let mut states = Vec::new();
        let edges = create_cross_edges(s);
        // Added -> base, base -> base and a base self-loop; a second drop is a no-op.
        for e in [edges[0], edges[3], edges[4]] {
            s.drop_edge(e).unwrap();
            s.drop_edge(e).unwrap();
            assert!(!s.contains_edge(e).unwrap());
            assert_eq!(s.edge_endpoints(e).unwrap(), None);
        }
        states.push(state(s));
        // An added vertex takes its edges to and from base vertices with it.
        s.drop_vertex(vertex_of(s, "n1")).unwrap();
        for e in [edges[1], edges[6], edges[7]] {
            assert!(!s.contains_edge(e).unwrap());
        }
        states.push(state(s));
        // New edges at the same base vertices again (vadas has none left), reusing slots; the
        // stale handles never reach the new occupants.
        let (marko, vadas) = (vertex_of(s, "1"), vertex_of(s, "2"));
        let n3 = s
            .insert_vertex(None, vec!["person".into()], PropertyMap::default())
            .unwrap();
        s.insert_edge(
            None,
            Some("self".into()),
            vadas,
            vadas,
            PropertyMap::default(),
        )
        .unwrap();
        s.insert_edge(
            None,
            Some("knows".into()),
            vadas,
            n3,
            PropertyMap::default(),
        )
        .unwrap();
        s.insert_edge(
            None,
            Some("knows".into()),
            n3,
            marko,
            PropertyMap::default(),
        )
        .unwrap();
        for e in [edges[0], edges[1], edges[3], edges[4]] {
            assert!(!s.contains_edge(e).unwrap());
            assert_eq!(s.edge_id(e).unwrap(), None);
        }
        states.push(state(s));
        // Everything new dropped: the base vertices read exactly as in the base again.
        let new_edges: Vec<S::EdgeId> = s
            .edges()
            .unwrap()
            .filter(|e| !["0", "1", "2", "3", "4", "5"].contains(&s.edge_id(*e).unwrap().unwrap()))
            .collect();
        for e in new_edges {
            s.drop_edge(e).unwrap();
        }
        s.drop_vertex(n3).unwrap();
        states.push(state(s));
        states
    }
    let mut layer = modern_layer();
    let on_layer = run(&mut layer);
    let on_reference = run(&mut TraversalGraph::tinkerpop_modern());
    assert_eq!(on_layer, on_reference);
    // The last state is the base's own adjacency again (the empty stubs stay, invisible).
    let base = TraversalGraph::tinkerpop_modern();
    let last = on_layer.last().unwrap();
    let base_state = state(&base);
    let adjacency = |s: &State| -> Vec<(String, Vec<String>, Vec<String>)> {
        s.vertices
            .iter()
            .filter(|v| ["1", "2", "3", "4", "5", "6"].contains(&v.0.as_str()))
            .map(|v| (v.0.clone(), v.3.clone(), v.4.clone()))
            .collect()
    };
    assert_eq!(adjacency(last), adjacency(&base_state));
    assert_eq!(layer.vertex_count().unwrap(), 7, "n2 stays");
    assert_eq!(layer.vertices().unwrap().count(), 7);
}

/// Closed-mode topology through new edges at base vertices: the same outcomes as a
/// `TraversalGraph` with the same schema and data, for edge creates with base endpoints and for
/// label changes of an added vertex whose incident edges lead to base vertices.
#[test]
fn closed_schema_topology_covers_edges_at_base_vertices() {
    const SCHEMA: &str = r#"{
      "mode": "closed",
      "vertices": {
        "person": {"schema": {"type": "object", "properties": {"name": {"type": "string"}}}},
        "software": {"schema": {"type": "object", "properties": {"name": {"type": "string"}}}}
      },
      "edges": {
        "knows": {"connections": [{"from": "person", "to": "person"}]},
        "created": {"connections": [{"from": "person", "to": "software"}]}
      }
    }"#;
    fn graph() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        graph
            .set_schema(GraphSchema::from_json(SCHEMA).unwrap())
            .unwrap();
        graph
            .insert_vertex(
                Some("1".into()),
                vec!["person".into()],
                props(&[("name", "marko".into())]),
            )
            .unwrap();
        graph
            .insert_vertex(
                Some("3".into()),
                vec!["software".into()],
                props(&[("name", "lop".into())]),
            )
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
        let (marko, lop) = (vertex_of(s, "1"), vertex_of(s, "3"));
        let p = s
            .insert_vertex(None, vec!["person".into()], props(&[("name", "p".into())]))
            .unwrap();
        let w = s
            .insert_vertex(
                None,
                vec!["software".into()],
                props(&[("name", "w".into())]),
            )
            .unwrap();
        let none = PropertyMap::default;
        for (label, from, to) in [
            ("knows", marko, p),
            ("knows", p, marko),
            ("created", marko, w),
            ("created", p, lop),
            ("knows", marko, lop),
            ("knows", w, marko),
            ("created", lop, p),
            ("created", marko, marko),
        ] {
            record(
                s.insert_edge(None, Some(label.into()), from, to, none())
                    .map(|_| "ok".into()),
            );
        }
        // `p` is linked to marko by `knows` both ways: it cannot stop being a person.
        record(
            s.add_vertex_label(p, "software".into())
                .map(|b| b.to_string()),
        );
        record(s.remove_vertex_label(p, "person").map(|b| b.to_string()));
        // `w` is the target of `created` from marko: it cannot become a person only.
        record(
            s.add_vertex_label(w, "person".into())
                .map(|b| b.to_string()),
        );
        record(s.remove_vertex_label(w, "software").map(|b| b.to_string()));
        (outcomes, state(s))
    }
    let on_layer = run(&mut LayeredGraph::over(Arc::new(graph())).unwrap());
    let on_reference = run(&mut graph());
    assert_eq!(on_layer, on_reference);
    let outcomes = &on_layer.0;
    assert_eq!(&outcomes[..4], ["ok", "ok", "ok", "ok"], "{outcomes:?}");
    assert!(
        outcomes[4..8].iter().all(|o| o != "ok"),
        "violations: {outcomes:?}"
    );
    assert!(
        outcomes[8..]
            .iter()
            .any(|o| o.contains("Topology") || o.contains("topology")),
        "a label change must hit the incident topology: {outcomes:?}"
    );
}

/// Scripts that add edges touching base vertices and then traverse: the same result and the same
/// graph state (with every vertex's adjacency in order) on the plain graph and on the layer.
#[cfg(feature = "script")]
const RHAI_CROSS_EDGES: &[&str] = &[
    r#"g.V("1").addE("knows").to(__.V("3")).iterate(); g.V("1").out("knows").values("name").toList()"#,
    r#"let a = g.addV("person").property("name", "ann").id().next(); g.V("4").addE("knows").to(__.V(a)).iterate(); [g.V("4").out().values("name").toList(), g.V(a).in().values("name").toList(), g.V(a).both().count().next()]"#,
    r#"let a = g.addV("person").property("name", "ann").id().next(); g.V(a).addE("created").to(__.V("5")).iterate(); [g.V("5").in("created").values("name").toList(), g.V("5").inE().count().next()]"#,
    r#"g.V("2").addE("self").to(__.V("2")).iterate(); [g.V("2").out("self").id().toList(), g.V("2").both().count().next(), g.V("2").bothE().id().toList()]"#,
    r#"g.V("6").addE("knows").to(__.V("1")).iterate(); g.V("6").out().out().path().by("name").toList()"#,
    r#"g.V("1").addE("likes").to(__.V("2")).iterate(); g.V("3").addE("likes").to(__.V("2")).iterate(); g.V().out("likes").groupCount().by("name").next()"#,
    r#"g.V("1").addE("x").to(__.V("1")).iterate(); [g.E().count().next(), g.V().count().next(), g.V().bothE().count().next(), g.E().hasLabel("x").count().next()]"#,
    r#"g.V("1").addE("x").to(__.V("2")).iterate(); g.E().hasLabel("x").drop().iterate(); [g.V("1").outE().id().toList(), g.E().count().next()]"#,
    r#"let a = g.addV("t").id().next(); g.V(a).addE("x").to(__.V("1")).iterate(); g.V("2").addE("y").to(__.V(a)).iterate(); g.V(a).drop().iterate(); [g.V("1").inE().count().next(), g.V("2").outE().count().next(), g.E().count().next(), g.V().count().next()]"#,
    r#"g.V("4").addE("k").to(__.V("1")).iterate(); [g.V("4").bothE().id().toList(), g.V("1").both().id().toList()]"#,
    r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "6"}).iterate(); g.V("1").out("knows").id().toList()"#,
    r#"g.V("1").as("a").V("3").addE("liked_by").to("a").iterate(); g.V("1").in("liked_by").values("name").toList()"#,
    r#"g.V("5").addE("next").to(__.V("1")).iterate(); g.V("1").repeat(__.out()).times(3).path().by("name").toList()"#,
    r#"g.V().hasLabel("person").addE("member").to(__.V("3")).iterate(); [g.V("3").in("member").values("name").order().toList(), g.V().group().by(T.label).by(__.bothE().count()).next()]"#,
    r#"g.V("1").addE("x").to(__.V("2")).property("w", 1).iterate(); g.V("1").outE("x").inV().values("name").toList()"#,
];

#[cfg(feature = "script")]
const CROSS_STATE_SCRIPT: &str = r#"[
    g.V().elementMap().toList(),
    g.E().elementMap().toList(),
    g.V().project("v", "out", "in").by(T.id).by(__.outE().id().fold()).by(__.inE().id().fold()).toList(),
    g.statistics()
]"#;

#[cfg(feature = "script")]
#[test]
fn rhai_cross_edge_scripts_match_the_plain_graph() {
    let mut differences = Vec::new();
    for script in RHAI_CROSS_EDGES {
        let plain = plain_graph();
        let layered = layered_graph();
        let expected = render(Arc::clone(&plain), script);
        let actual = render(Arc::clone(&layered), script);
        assert!(expected.is_ok(), "{script}: {expected:?}");
        let expected_state = render(plain, CROSS_STATE_SCRIPT);
        let actual_state = render(layered, CROSS_STATE_SCRIPT);
        if expected != actual || expected_state != actual_state {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}\n  state TraversalGraph: {expected_state:?}\n  state LayeredGraph:   {actual_state:?}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// A stale endpoint fails as in a `TraversalGraph`, and a failed edge create leaves no trace
/// (no stub, no dirty base vertex: its lists read exactly as before).
#[test]
fn failed_edge_creates_at_base_vertices_leave_no_trace() {
    let mut layer = modern_layer();
    let marko = v(&layer, "1");
    let new = layer
        .insert_vertex(None, vec!["person".into()], PropertyMap::default())
        .unwrap();
    layer.drop_vertex(new).unwrap();
    let before = state(&layer);
    let err = layer
        .insert_edge(None, None, new, marko, PropertyMap::default())
        .unwrap_err();
    let mut reference = TraversalGraph::tinkerpop_modern();
    let gone = reference
        .insert_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    reference.drop_vertex(gone).unwrap();
    let marko_ref = vertex_of(&reference, "1");
    let expected = reference
        .insert_edge(None, None, gone, marko_ref, PropertyMap::default())
        .unwrap_err();
    assert_eq!(format!("{err:?}"), format!("{expected:?}"));
    // A taken edge id: refused after the rule, before any stub is made.
    let err = layer
        .insert_edge(Some("1".into()), None, marko, marko, PropertyMap::default())
        .unwrap_err();
    let expected = reference
        .insert_edge(
            Some("1".into()),
            None,
            marko_ref,
            marko_ref,
            PropertyMap::default(),
        )
        .unwrap_err();
    assert_eq!(format!("{err:?}"), format!("{expected:?}"));
    assert_eq!(state(&layer), before);
}

// ------------------------------------------------------------------------------------------
// Drops of base elements: tombstones (S1.5)
// ------------------------------------------------------------------------------------------

/// `state` with its scans sorted by id (adjacency lists keep their order): the comparison of
/// plan decision P4 for a storage that dropped elements and then added new ones, where a
/// `TraversalGraph` reuses the freed slots (LIFO) and a layer appends.
fn sorted_state<S: GraphStorage>(s: &S) -> State {
    let mut state = state(s).with_sorted_buckets();
    state.vertices.sort_by(|a, b| a.0.cmp(&b.0));
    state.edges.sort_by(|a, b| a.0.cmp(&b.0));
    state
}

impl State {
    /// The state with every label bucket sorted: a `TraversalGraph` reorders a bucket when it
    /// removes a member (see `label_lookups_after_a_drop_differ_only_in_order`).
    fn with_sorted_buckets(mut self) -> Self {
        for (_, vs, es) in &mut self.by_label {
            vs.sort();
            es.sort();
        }
        self
    }
}

/// The edge with `id` on any storage.
fn edge_of<S: GraphStorage>(s: &S, id: &str) -> S::EdgeId {
    s.get_edge_by_id(id).unwrap().unwrap()
}

/// Drops of base elements over modern with new edges at base vertices, the state after each
/// step: a base edge; a base vertex with base edges in both directions, added edges at its stub
/// (in and out) and an added self-loop; a base vertex with a base self-loop the layer added
/// edges to; an idempotent second drop of each.
fn drop_base_elements<S: GraphStorage>(s: &mut S) -> Vec<State> {
    let mut states = Vec::new();
    create_cross_edges(s);
    let josh_lop = edge_of(s, "3");
    s.drop_edge(josh_lop).unwrap();
    s.drop_edge(josh_lop).unwrap();
    states.push(state(s).with_sorted_buckets());
    let marko = vertex_of(s, "1");
    let self_loop = s
        .insert_edge(
            None,
            Some("self".into()),
            marko,
            marko,
            PropertyMap::default(),
        )
        .unwrap();
    s.drop_vertex(marko).unwrap();
    s.drop_vertex(marko).unwrap();
    assert!(!s.contains_edge(self_loop).unwrap());
    states.push(state(s).with_sorted_buckets());
    let vadas = vertex_of(s, "2");
    s.drop_vertex(vadas).unwrap();
    states.push(state(s).with_sorted_buckets());
    // Every other element of the base, then what is left.
    for id in ["3", "4", "5", "6"] {
        let node = vertex_of(s, id);
        s.drop_vertex(node).unwrap();
    }
    states.push(state(s).with_sorted_buckets());
    states
}

#[test]
fn drops_of_base_elements_read_like_a_traversal_graph() {
    let base = modern_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let mut reference = TraversalGraph::tinkerpop_modern();
    let on_layer = drop_base_elements(&mut layer);
    let on_reference = drop_base_elements(&mut reference);
    assert_eq!(on_layer.len(), on_reference.len());
    for (step, (a, b)) in on_layer.iter().zip(&on_reference).enumerate() {
        assert_eq!(a, b, "step {step}");
    }
    // Only the two added vertices survive, with the one added edge between them.
    assert_eq!(on_layer[3].counts, (2, 2));
    // The base is unchanged; another layer over it sees everything.
    assert_eq!(TraversalGraph::vertex_count(&base), 6);
    let other = LayeredGraph::over(base).unwrap();
    assert_eq!(other.vertex_count().unwrap(), 6);
    assert_eq!(other.out_edges(v(&other, "1")).unwrap().count(), 3);
}

/// A base with a self-loop and edges in both directions: dropping the looped vertex drops the
/// loop once and the other vertex keeps clean reads of what is left.
#[test]
fn drop_of_a_base_vertex_with_a_base_self_loop() {
    fn base() -> TraversalGraph {
        let mut g = TraversalGraph::new();
        let a = g
            .insert_vertex(Some("a".into()), vec!["n".into()], PropertyMap::default())
            .unwrap();
        let b = g
            .insert_vertex(Some("b".into()), vec!["n".into()], PropertyMap::default())
            .unwrap();
        let c = g
            .insert_vertex(Some("c".into()), vec!["m".into()], PropertyMap::default())
            .unwrap();
        for (id, from, to) in [("aa", a, a), ("ab", a, b), ("ba", b, a), ("bc", b, c)] {
            g.insert_edge(
                Some(id.into()),
                Some("e".into()),
                from,
                to,
                PropertyMap::default(),
            )
            .unwrap();
        }
        g
    }
    fn run<S: GraphStorage>(s: &mut S) -> Vec<State> {
        let mut states = Vec::new();
        let a = vertex_of(s, "a");
        s.drop_vertex(a).unwrap();
        states.push(state(s).with_sorted_buckets());
        // A base edge between two live vertices; the self-loop of a base edge drop.
        let c = vertex_of(s, "c");
        s.insert_edge(
            Some("cc".into()),
            Some("e".into()),
            c,
            c,
            PropertyMap::default(),
        )
        .unwrap();
        let bc = edge_of(s, "bc");
        s.drop_edge(bc).unwrap();
        states.push(state(s).with_sorted_buckets());
        states
    }
    let mut layer = LayeredGraph::over(Arc::new(base())).unwrap();
    let mut reference = base();
    assert_eq!(run(&mut layer), run(&mut reference));
    assert_eq!(layer.vertex_count().unwrap(), 2);
    assert_eq!(layer.edge_count().unwrap(), 1);
}

/// A dropped base id is not found, and is free for a new element of the layer, which is found
/// by it; a stale handle of the dropped element never reaches the new one. Order is compared
/// per P4 (sorted scans): the `TraversalGraph` reuses the freed slots.
#[test]
fn a_dropped_base_id_can_be_reused() {
    fn run<S: GraphStorage>(s: &mut S) -> (State, S::VertexId, S::EdgeId) {
        let marko = vertex_of(s, "1");
        let knows = edge_of(s, "0");
        s.drop_edge(knows).unwrap();
        assert_eq!(s.get_edge_by_id("0").unwrap(), None);
        s.drop_vertex(marko).unwrap();
        assert_eq!(s.get_vertex_by_id("1").unwrap(), None);
        for id in ["0", "1", "2"] {
            assert_eq!(s.get_edge_by_id(id).unwrap(), None, "{id}");
        }
        let new = s
            .insert_vertex(
                Some("1".into()),
                vec!["person".into()],
                props(&[("name", "new".into())]),
            )
            .unwrap();
        let vadas = vertex_of(s, "2");
        let edge = s
            .insert_edge(
                Some("0".into()),
                Some("knows".into()),
                new,
                vadas,
                PropertyMap::default(),
            )
            .unwrap();
        assert_eq!(s.get_vertex_by_id("1").unwrap(), Some(new));
        assert_eq!(s.get_edge_by_id("0").unwrap(), Some(edge));
        assert_ne!(new, marko);
        assert!(!s.contains_vertex(marko).unwrap());
        assert!(!s.contains_edge(knows).unwrap());
        assert_eq!(s.vertex_id(marko).unwrap(), None);
        assert_eq!(s.edge_id(knows).unwrap(), None);
        // A taken id is still refused: the new vertex holds it now.
        assert!(
            s.insert_vertex(Some("1".into()), vec![], PropertyMap::default())
                .is_err()
        );
        // Auto ids continue the sequence as in a `TraversalGraph`.
        s.insert_vertex(None, vec!["person".into()], PropertyMap::default())
            .unwrap();
        s.insert_edge(None, None, new, new, PropertyMap::default())
            .unwrap();
        (sorted_state(s), marko, knows)
    }
    let mut layer = modern_layer();
    let mut reference = TraversalGraph::tinkerpop_modern();
    let (on_layer, ..) = run(&mut layer);
    let (on_reference, ..) = run(&mut reference);
    assert_eq!(on_layer, on_reference);
}

/// A handle of a dropped base element reads like a removed element of a `TraversalGraph`
/// (nothing, no lists, no endpoints) and every write on it fails with the same error, the
/// write rule deciding first; a second drop is a no-op.
#[test]
fn stale_handles_of_dropped_base_elements_answer_like_a_traversal_graph() {
    fn outcomes<S: GraphStorage>(s: &mut S) -> Vec<String> {
        let marko = vertex_of(s, "1");
        let josh = vertex_of(s, "4");
        let knows = edge_of(s, "0");
        let created = edge_of(s, "3");
        s.drop_vertex(marko).unwrap();
        s.drop_edge(created).unwrap();
        let mut out = Vec::new();
        for node in [marko] {
            out.push(format!(
                "{:?} {:?} {:?} {:?} {:?} {} {}",
                s.contains_vertex(node),
                s.vertex_id(node),
                s.vertex_label(node),
                s.vertex_labels(node),
                s.vertex_properties(node),
                s.out_edges(node).unwrap().count(),
                s.in_edges(node).unwrap().count(),
            ));
            out.push(format!("{:?}", s.add_vertex_label(node, "x".into())));
            out.push(format!("{:?}", s.remove_vertex_label(node, "person")));
            out.push(format!(
                "{:?}",
                s.set_vertex_property(node, "age".into(), 1.into())
            ));
            out.push(format!("{:?}", s.drop_vertex_property("age", node)));
            out.push(format!(
                "{:?}",
                s.insert_edge(None, Some("x".into()), node, josh, PropertyMap::default())
            ));
            out.push(format!(
                "{:?}",
                s.insert_edge(None, Some("x".into()), josh, node, PropertyMap::default())
            ));
            out.push(format!("{:?}", s.drop_vertex(node)));
        }
        for edge in [knows, created] {
            out.push(format!(
                "{:?} {:?} {:?} {:?}",
                s.contains_edge(edge),
                s.edge_id(edge),
                s.edge_label(edge),
                s.edge_properties(edge),
            ));
            out.push(format!("{}", s.edge_endpoints(edge).unwrap().is_none()));
            out.push(format!("{:?}", s.set_edge_label(edge, "x".into())));
            out.push(format!(
                "{:?}",
                s.set_edge_property(edge, "weight".into(), 1.into())
            ));
            out.push(format!("{:?}", s.drop_edge_property("weight", edge)));
            out.push(format!("{:?}", s.drop_edge(edge)));
        }
        out
    }
    let mut layer = modern_layer();
    let mut reference = TraversalGraph::tinkerpop_modern();
    let on_layer = outcomes(&mut layer);
    let on_reference = outcomes(&mut reference);
    assert_eq!(on_layer, on_reference);
    assert!(
        on_layer.iter().any(|o| o.contains("Removed")),
        "{on_layer:?}"
    );
    // The failed writes changed nothing: both hold the same state.
    assert_eq!(
        state(&layer).with_sorted_buckets(),
        state(&reference).with_sorted_buckets()
    );
    assert_eq!(layer.get_vertex_by_id("1").unwrap(), None);
    assert_eq!(layer.get_edge_by_id("3").unwrap(), None);
}

/// Live base vertices keep reading as the base holds them after a neighbour or an incident
/// edge was dropped; only their lists lose the removed edges. Label lookups, counts and the
/// typed traversal API agree.
#[test]
fn reads_around_dropped_base_elements() {
    fn run<S: GraphStorage>(s: &mut S) -> Vec<Vec<String>> {
        let josh_lop = edge_of(s, "3");
        s.drop_edge(josh_lop).unwrap();
        let vadas = vertex_of(s, "2");
        s.drop_vertex(vadas).unwrap();
        let eids = |edges: Vec<S::EdgeId>| -> Vec<String> {
            edges
                .into_iter()
                .map(|e| s.edge_id(e).unwrap().unwrap().to_string())
                .collect()
        };
        let mut lists = Vec::new();
        for id in ["1", "3", "4", "5", "6"] {
            let node = vertex_of(s, id);
            lists.push(eids(s.out_edges(node).unwrap().collect()));
            lists.push(eids(s.in_edges(node).unwrap().collect()));
        }
        lists
    }
    let mut layer = modern_layer();
    let mut reference = TraversalGraph::tinkerpop_modern();
    assert_eq!(run(&mut layer), run(&mut reference));
    let josh = v(&layer, "4");
    assert_eq!(layer.vertex_count().unwrap(), 5);
    assert_eq!(layer.edge_count().unwrap(), 4);
    let mut people: Vec<&str> = layer
        .vertices_by_label("person")
        .unwrap()
        .map(|n| layer.vertex_id(n).unwrap().unwrap())
        .collect();
    people.sort();
    assert_eq!(people, ["1", "4", "6"]);
    assert_eq!(layer.edges_by_label("knows").unwrap().count(), 1);
    assert_eq!(layer.edges_by_label("created").unwrap().count(), 3);
    assert_eq!(
        layer.vertex_properties(josh).unwrap().unwrap().get("name"),
        Some(&"josh".into())
    );
    let stats = layer.statistics().unwrap().unwrap();
    assert_eq!(stats.vertex_label_count("person"), 3);
    assert_eq!(stats.edge_label_count("knows"), 1);
    assert_eq!(stats, &GraphStatistics::from_storage(&layer).unwrap());
    assert_eq!(
        state(&layer).with_sorted_buckets(),
        state(&reference).with_sorted_buckets()
    );
    assert_eq!(
        GraphTraversalSource::new(&layer)
            .v(None)
            .values("name")
            .to_list()
            .unwrap(),
        GraphTraversalSource::new(&reference)
            .v(None)
            .values("name")
            .to_list()
            .unwrap()
    );
}

/// The one order difference of a drop without a later add (plan decision P4): a
/// `TraversalGraph` removes a member from a label bucket by moving the bucket's last member
/// into its place, the layer keeps the shared base bucket's order and skips the removed one.
/// The same members either way.
#[test]
fn label_lookups_after_a_drop_differ_only_in_order() {
    fn people<S: GraphStorage>(s: &mut S) -> Vec<String> {
        let marko = vertex_of(s, "1");
        s.drop_vertex(marko).unwrap();
        s.vertices_by_label("person")
            .unwrap()
            .map(|n| s.vertex_id(n).unwrap().unwrap().to_string())
            .collect()
    }
    let on_layer = people(&mut modern_layer());
    let on_reference = people(&mut TraversalGraph::tinkerpop_modern());
    assert_eq!(on_layer, ["2", "4", "6"]);
    assert_eq!(on_reference, ["6", "2", "4"]);
}

/// Scripts that drop base elements and then traverse, count, group: the same result and the
/// same graph state on the plain graph and on the layer. Exact, order included.
#[cfg(feature = "script")]
const RHAI_DROPS: &[&str] = &[
    r#"g.V("1").drop().iterate(); [g.V().count().next(), g.E().count().next(), g.V().values("name").toList()]"#,
    r#"g.E("0").drop().iterate(); g.V("1").out().values("name").toList()"#,
    r#"g.V().hasLabel("software").drop().iterate(); g.V().groupCount().by(T.label).next()"#,
    r#"g.V("4").drop().iterate(); [g.V("1").out("knows").values("name").toList(), g.V("5").in().count().next(), g.E().groupCount().by(T.label).next()]"#,
    r#"g.E().hasLabel("created").drop().iterate(); [g.V().out().path().by("name").toList(), g.statistics()]"#,
    r#"g.V("1").addE("self").to(__.V("1")).iterate(); g.V("1").drop().iterate(); [g.E().count().next(), g.V().bothE().count().next()]"#,
    r#"let a = g.addV("t").id().next(); g.V(a).addE("x").to(__.V("1")).iterate(); g.V("2").addE("y").to(__.V("1")).iterate(); g.V("1").drop().iterate(); [g.V(a).outE().count().next(), g.V("2").outE().count().next(), g.E().count().next(), g.V().count().next()]"#,
    r#"g.V("1").drop().iterate(); [g.V("1").count().next(), g.E("0").count().next(), g.V().hasId("1").count().next()]"#,
    r#"g.V("1").drop().iterate(); g.V("1").drop().iterate(); g.E("5").drop().iterate(); g.E("5").drop().iterate(); [g.V().count().next(), g.E().count().next()]"#,
    r#"g.V().drop().iterate(); [g.V().count().next(), g.E().count().next(), g.statistics()]"#,
    r#"g.V("3").bothE().drop().iterate(); g.V().group().by(T.label).by(__.bothE().count()).next()"#,
    r#"g.V().has("age", gt(30)).drop().iterate(); g.V().values("name").toList()"#,
    r#"g.V("2").drop().iterate(); [g.V("1").out("knows").values("name").toList(), g.V("1").outE().id().toList()]"#,
    r#"g.V("1").outE("knows").drop().iterate(); g.V().groupCount().by(__.bothE().count()).next()"#,
    r#"g.V("4").outE().drop().iterate(); g.V("1").repeat(__.out()).emit().path().by("name").toList()"#,
    r#"g.V("6").drop().iterate(); g.V().hasLabel("person").order().by("age").values("name").toList()"#,
    r#"g.V("1").addE("k").to(__.V("6")).iterate(); g.E("1").drop().iterate(); [g.V("1").outE().id().toList(), g.V("6").inE().count().next()]"#,
    r#"g.V().hasLabel("person").outE("created").drop().iterate(); [g.V("3").in().count().next(), g.E().label().dedup().toList()]"#,
];

/// Scripts that drop base elements and then add new ones. Plan decision P4: compared exactly
/// first; where only the order of the state differs (the `TraversalGraph` reuses freed slots,
/// the layer appends) the state is compared sorted by id. Each script's own result is
/// order-independent.
#[cfg(feature = "script")]
const RHAI_DROP_THEN_ADD: &[&str] = &[
    r#"g.V("1").drop().iterate(); g.addV("person").property(T.id, "1").property("name", "new").iterate(); [g.V("1").values("name").next(), g.V("1").outE().count().next(), g.V().count().next()]"#,
    r#"g.E("0").drop().iterate(); g.V("1").addE("knows").property(T.id, "0").to(__.V("2")).iterate(); [g.E("0").inV().values("name").next(), g.V("1").outE().id().toList()]"#,
    r#"g.V("6").drop().iterate(); g.addV("person").property("name", "z").iterate(); g.V().hasLabel("person").values("name").order().toList()"#,
    r#"g.V("1").outE("knows").drop().iterate(); g.V("1").as("a").V("2").addE("back").to("a").iterate(); g.V("1").bothE().label().order().toList()"#,
    r#"g.V().drop().iterate(); g.addV("a").property(T.id, "1").as("x").addV("b").property(T.id, "2").addE("e").to("x").iterate(); [g.V().id().order().toList(), g.E().count().next(), g.statistics()]"#,
    r#"g.V("3").drop().iterate(); g.addV("software").property("name", "lop2").as("s").V("4").addE("created").to("s").iterate(); g.V("4").out("created").values("name").order().toList()"#,
];

/// Scripts that drop base elements and then read by label, each with its sorted form. The
/// label lookup lists the same members in another order (`TraversalGraph` moves a bucket's last
/// member into a removed one's place, the layer keeps the base bucket's order: plan decision
/// P4, see `label_lookups_after_a_drop_differ_only_in_order`): the sorted forms and the state
/// match exactly.
#[cfg(feature = "script")]
const RHAI_DROP_LABEL_ORDER: &[(&str, &str)] = &[
    (
        r#"g.V("1").drop().iterate(); g.V().hasLabel("person").values("name").toList()"#,
        r#"g.V("1").drop().iterate(); g.V().hasLabel("person").values("name").order().toList()"#,
    ),
    (
        r#"g.E("0").drop().iterate(); g.E().hasLabel("knows", "created").id().toList()"#,
        r#"g.E("0").drop().iterate(); g.E().hasLabel("knows", "created").id().order().toList()"#,
    ),
    (
        r#"g.V("3").drop().iterate(); g.V().hasLabel("software").in("created").values("name").toList()"#,
        r#"g.V("3").drop().iterate(); g.V().hasLabel("software").in("created").values("name").order().toList()"#,
    ),
];

#[cfg(feature = "script")]
#[test]
fn rhai_label_lookups_after_drops_match_up_to_order() {
    let mut differences = Vec::new();
    let mut order_only = Vec::new();
    for (script, sorted) in RHAI_DROP_LABEL_ORDER {
        let plain = plain_graph();
        let layered = layered_graph();
        let expected = render(Arc::clone(&plain), sorted);
        let actual = render(Arc::clone(&layered), sorted);
        assert!(expected.is_ok(), "{sorted}: {expected:?}");
        let expected_state = render(Arc::clone(&plain), CROSS_STATE_SCRIPT);
        let actual_state = render(Arc::clone(&layered), CROSS_STATE_SCRIPT);
        if expected != actual || expected_state != actual_state {
            differences.push(format!(
                "`{sorted}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}"
            ));
        }
        if render(plain_graph(), script) != render(layered_graph(), script) {
            order_only.push(*script);
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    println!(
        "order-only differences of label lookups (P4): {}\n{}",
        order_only.len(),
        order_only.join("\n")
    );
}

#[cfg(feature = "script")]
const SORTED_STATE_SCRIPT: &str = r#"[
    g.V().order().by(T.id).elementMap().toList(),
    g.E().order().by(T.id).elementMap().toList(),
    g.V().order().by(T.id).project("v", "out", "in").by(T.id).by(__.outE().id().fold()).by(__.inE().id().fold()).toList(),
    g.statistics()
]"#;

#[cfg(feature = "script")]
#[test]
fn rhai_drop_scripts_match_the_plain_graph() {
    let mut differences = Vec::new();
    let mut order_only = Vec::new();
    for (script, may_reorder) in RHAI_DROPS
        .iter()
        .map(|s| (s, false))
        .chain(RHAI_DROP_THEN_ADD.iter().map(|s| (s, true)))
    {
        let plain = plain_graph();
        let layered = layered_graph();
        let expected = render(Arc::clone(&plain), script);
        let actual = render(Arc::clone(&layered), script);
        assert!(expected.is_ok(), "{script}: {expected:?}");
        let expected_state = render(Arc::clone(&plain), CROSS_STATE_SCRIPT);
        let actual_state = render(Arc::clone(&layered), CROSS_STATE_SCRIPT);
        if expected == actual && expected_state == actual_state {
            continue;
        }
        if may_reorder
            && expected == actual
            && render(plain, SORTED_STATE_SCRIPT) == render(layered, SORTED_STATE_SCRIPT)
        {
            order_only.push(*script);
            continue;
        }
        differences.push(format!(
            "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}\n  state TraversalGraph: {expected_state:?}\n  state LayeredGraph:   {actual_state:?}"
        ));
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    println!(
        "order-only differences (P4): {}\n{}",
        order_only.len(),
        order_only.join("\n")
    );
}

// ------------------------------------------------------------------------------------------
// Writes to base elements: copy-up images (S1.6)
// ------------------------------------------------------------------------------------------

fn path(text: &str) -> JsonPath {
    text.parse().unwrap()
}

/// Property writes to base vertices and edges over modern (plain sets, new keys, drops, path
/// writes and removals, a written vertex with an added edge, drops of written elements), the
/// state after each step.
fn write_base_properties<S: GraphStorage>(s: &mut S) -> Vec<State> {
    let mut states = Vec::new();
    let marko = vertex_of(s, "1");
    let vadas = vertex_of(s, "2");
    let josh = vertex_of(s, "4");
    let knows = edge_of(s, "0");
    let created = edge_of(s, "3");
    s.set_vertex_property(marko, "age".into(), 30.into())
        .unwrap();
    states.push(state(s));
    s.set_vertex_property(marko, "nick".into(), "mk".into())
        .unwrap();
    s.drop_vertex_property("age", vadas).unwrap();
    states.push(state(s));
    s.set_edge_property(knows, "weight".into(), 0.9.into())
        .unwrap();
    s.set_edge_property(knows, "since".into(), 2010.into())
        .unwrap();
    s.drop_edge_property("weight", created).unwrap();
    states.push(state(s));
    s.set_vertex_property_path(vadas, &path("meta.tags"), vec!["a"].into())
        .unwrap();
    s.set_vertex_property_path(vadas, &path("meta.tags[1]"), "b".into())
        .unwrap();
    s.set_edge_property_path(created, &path("info.by"), "x".into())
        .unwrap();
    states.push(state(s));
    s.remove_vertex_property_path(vadas, &path("meta.tags[0]"))
        .unwrap();
    s.remove_edge_property_path(created, &path("info.by"))
        .unwrap();
    s.remove_vertex_property_path(vadas, &path("missing.key"))
        .unwrap();
    states.push(state(s));
    s.remove_vertex_property_path(vadas, &path("meta")).unwrap();
    states.push(state(s));
    // Every property of josh removed, then one set again.
    for key in ["name", "age"] {
        s.drop_vertex_property(key, josh).unwrap();
    }
    s.set_vertex_property(josh, "age".into(), 32.into())
        .unwrap();
    states.push(state(s));
    // A written vertex with an added edge, written again; then written elements dropped.
    let n = s
        .insert_vertex(None, vec!["n".into()], PropertyMap::default())
        .unwrap();
    s.insert_edge(None, Some("e".into()), marko, n, PropertyMap::default())
        .unwrap();
    s.set_vertex_property(marko, "age".into(), 31.into())
        .unwrap();
    states.push(state(s));
    // A drop moves a bucket's last member in a `TraversalGraph` (plan P4): sorted buckets.
    s.drop_edge(created).unwrap();
    s.drop_vertex(marko).unwrap();
    states.push(state(s).with_sorted_buckets());
    states
}

#[test]
fn property_writes_on_base_elements_read_like_a_traversal_graph() {
    let base = modern_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let on_layer = write_base_properties(&mut layer);
    let on_reference = write_base_properties(&mut TraversalGraph::tinkerpop_modern());
    assert_eq!(on_layer.len(), on_reference.len());
    for (step, (l, r)) in on_layer.iter().zip(&on_reference).enumerate() {
        assert_eq!(l, r, "step {step}");
    }
    // The shared base is unchanged; a second layer over it sees none of it.
    let untouched = state(&*base);
    assert_eq!(untouched, state(&TraversalGraph::tinkerpop_modern()));
    assert_eq!(state(&modern_layer()), untouched);
    assert_eq!(state(&LayeredGraph::over(base).unwrap()), untouched);
}

/// Label writes to base vertices and edges that only ADD memberships (labels gained, edges
/// relabeled to a label): exact, label-bucket order included, interleaved with added
/// elements of the same label as a `TraversalGraph` appends both.
fn add_base_labels<S: GraphStorage>(s: &mut S) -> Vec<State> {
    let mut states = Vec::new();
    let marko = vertex_of(s, "1");
    let vadas = vertex_of(s, "2");
    let lop = vertex_of(s, "3");
    assert!(s.add_vertex_label(marko, "employee".into()).unwrap());
    states.push(state(s));
    s.insert_vertex(
        Some("e1".into()),
        vec!["employee".into()],
        PropertyMap::default(),
    )
    .unwrap();
    assert!(s.add_vertex_label(vadas, "employee".into()).unwrap());
    assert!(!s.add_vertex_label(vadas, "employee".into()).unwrap());
    assert!(!s.add_vertex_label(vadas, "person".into()).unwrap());
    assert!(s.add_vertex_label(lop, "person".into()).unwrap());
    states.push(state(s));
    let knows = edge_of(s, "0");
    let created = edge_of(s, "4");
    assert!(s.set_edge_label(knows, "met".into()).unwrap());
    assert!(!s.set_edge_label(knows, "met".into()).unwrap());
    assert!(s.set_edge_label(created, "knows".into()).unwrap());
    states.push(state(s));
    s.set_vertex_property(marko, "age".into(), 1.into())
        .unwrap();
    s.set_edge_property(knows, "weight".into(), 2.into())
        .unwrap();
    states.push(state(s));
    // A drop moves a bucket's last member in a `TraversalGraph` (plan P4): sorted buckets.
    s.drop_vertex(marko).unwrap();
    states.push(state(s).with_sorted_buckets());
    states
}

#[test]
fn label_additions_on_base_elements_read_like_a_traversal_graph() {
    let on_layer = add_base_labels(&mut modern_layer());
    let on_reference = add_base_labels(&mut TraversalGraph::tinkerpop_modern());
    for (step, (l, r)) in on_layer.iter().zip(&on_reference).enumerate() {
        assert_eq!(l, r, "step {step}");
    }
    let last = &on_layer[1].by_label;
    assert!(
        last.contains(&(
            "employee".into(),
            vec!["1".into(), "e1".into(), "2".into()],
            vec![]
        )),
        "{last:?}"
    );
}

/// Label writes that REMOVE memberships (labels dropped, edges relabeled away and back,
/// vertices relabeled back to their base labels): the same as a `TraversalGraph` with every
/// label bucket sorted. A `TraversalGraph` moves a bucket's last member into a removed one's
/// place (and appends one that comes back); the layer keeps the base bucket's order (plan P4,
/// the order class of S1.5).
fn change_base_labels<S: GraphStorage>(s: &mut S) -> Vec<State> {
    let mut states = Vec::new();
    let marko = vertex_of(s, "1");
    let vadas = vertex_of(s, "2");
    let lop = vertex_of(s, "3");
    assert!(s.remove_vertex_label(marko, "person").unwrap());
    assert!(!s.remove_vertex_label(marko, "person").unwrap());
    states.push(state(s).with_sorted_buckets());
    assert!(s.add_vertex_label(vadas, "x".into()).unwrap());
    assert!(s.add_vertex_label(vadas, "y".into()).unwrap());
    assert!(s.remove_vertex_label(vadas, "person").unwrap());
    assert!(s.remove_vertex_label(vadas, "x").unwrap());
    states.push(state(s).with_sorted_buckets());
    // Back to the base labels, in another order, then in the base order.
    assert!(s.add_vertex_label(marko, "person".into()).unwrap());
    assert!(s.add_vertex_label(lop, "a".into()).unwrap());
    assert!(s.remove_vertex_label(lop, "software").unwrap());
    assert!(s.add_vertex_label(lop, "software".into()).unwrap());
    states.push(state(s).with_sorted_buckets());
    assert!(s.remove_vertex_label(lop, "a").unwrap());
    states.push(state(s).with_sorted_buckets());
    let knows = edge_of(s, "0");
    assert!(s.set_edge_label(knows, "met".into()).unwrap());
    assert!(s.set_edge_label(knows, "created".into()).unwrap());
    states.push(state(s).with_sorted_buckets());
    assert!(s.set_edge_label(knows, "knows".into()).unwrap());
    states.push(state(s).with_sorted_buckets());
    s.insert_edge(
        None,
        Some("met".into()),
        marko,
        vadas,
        PropertyMap::default(),
    )
    .unwrap();
    assert!(s.set_edge_label(knows, "met".into()).unwrap());
    states.push(state(s).with_sorted_buckets());
    s.drop_edge(knows).unwrap();
    s.drop_vertex(vadas).unwrap();
    states.push(state(s).with_sorted_buckets());
    states
}

#[test]
fn label_removals_on_base_elements_match_up_to_bucket_order() {
    let on_layer = change_base_labels(&mut modern_layer());
    let on_reference = change_base_labels(&mut TraversalGraph::tinkerpop_modern());
    for (step, (l, r)) in on_layer.iter().zip(&on_reference).enumerate() {
        assert_eq!(l, r, "step {step}");
    }
}

/// The layer's base vertex `id` reads straight from the base (no image): its map and label
/// slice are the base's own.
fn reads_the_base(layer: &LayeredGraph, base: &TraversalGraph, id: &str) -> bool {
    let node = v(layer, id);
    let on_base = base.get_vertex_by_id(id).unwrap().unwrap();
    std::ptr::eq(
        layer.vertex_properties(node).unwrap().unwrap(),
        base.vertex_properties(on_base).unwrap().unwrap(),
    ) && std::ptr::eq(
        layer.vertex_labels(node).unwrap().as_ptr(),
        base.vertex_labels(on_base).unwrap().as_ptr(),
    )
}

/// The edge counterpart of [`reads_the_base`].
fn edge_reads_the_base(layer: &LayeredGraph, base: &TraversalGraph, id: &str) -> bool {
    let edge = edge_of(layer, id);
    let on_base = base.get_edge_by_id(id).unwrap().unwrap();
    std::ptr::eq(
        layer.edge_properties(edge).unwrap().unwrap(),
        base.edge_properties(on_base).unwrap().unwrap(),
    ) && std::ptr::eq(
        layer.edge_label(edge).unwrap().unwrap(),
        base.edge_label(on_base).unwrap().unwrap(),
    )
}

/// Plan P6: a write whose result is identical to the current value (`identity_eq`) creates no
/// image; `1` and `1.0` are not identical.
#[test]
fn equal_value_writes_create_no_image() {
    let base = modern_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let marko = v(&layer, "1");
    let knows = edge_of(&layer, "0");
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    layer
        .set_vertex_property(marko, "name".into(), "marko".into())
        .unwrap();
    layer.drop_vertex_property("missing", marko).unwrap();
    layer
        .set_vertex_property_path(marko, &path("name"), "marko".into())
        .unwrap();
    layer
        .remove_vertex_property_path(marko, &path("missing.key"))
        .unwrap();
    assert!(!layer.add_vertex_label(marko, "person".into()).unwrap());
    assert!(!layer.remove_vertex_label(marko, "nope").unwrap());
    layer
        .set_edge_property(knows, "weight".into(), 0.5.into())
        .unwrap();
    layer.drop_edge_property("missing", knows).unwrap();
    assert!(!layer.set_edge_label(knows, "knows".into()).unwrap());
    assert!(reads_the_base(&layer, &base, "1"));
    assert!(edge_reads_the_base(&layer, &base, "0"));
    assert_eq!(state(&layer), state(&*base));

    // 29.0 is not identical to 29: an image, holding the float.
    layer
        .set_vertex_property(marko, "age".into(), 29.0.into())
        .unwrap();
    assert!(!reads_the_base(&layer, &base, "1"));
    let age = layer.vertex_properties(marko).unwrap().unwrap()["age"].clone();
    assert!(
        matches!(&age, ElementProperty::Number(n) if n.is_f64()),
        "{age:?}"
    );
    // And back: the image is identical to the base again and dropped.
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    assert!(reads_the_base(&layer, &base, "1"));
    // The same for an edge: 1 replaces 1.0, then 1.0 again.
    let josh = edge_of(&layer, "2");
    layer
        .set_edge_property(josh, "weight".into(), 1.into())
        .unwrap();
    assert!(!edge_reads_the_base(&layer, &base, "2"));
    layer
        .set_edge_property(josh, "weight".into(), 1.0.into())
        .unwrap();
    assert!(edge_reads_the_base(&layer, &base, "2"));
    assert_eq!(state(&layer), state(&*base));
}

/// An image identical to its base element again is dropped (return to clean) after property,
/// path and label writes; key order counts (a key dropped and set again moves to the end,
/// as in a `TraversalGraph`), so such an image stays.
#[test]
fn an_image_equal_to_the_base_again_returns_to_clean() {
    let base = modern_base();
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let marko = v(&layer, "1");
    let vadas = v(&layer, "2");
    // Property set and set back.
    layer
        .set_vertex_property(marko, "age".into(), 1.into())
        .unwrap();
    layer
        .set_vertex_property(marko, "x".into(), 1.into())
        .unwrap();
    layer.drop_vertex_property("x", marko).unwrap();
    assert!(!reads_the_base(&layer, &base, "1"));
    layer
        .set_vertex_property(marko, "age".into(), 29.into())
        .unwrap();
    assert!(reads_the_base(&layer, &base, "1"));
    // Path write and removal.
    layer
        .set_vertex_property_path(vadas, &path("meta.a"), 1.into())
        .unwrap();
    layer
        .remove_vertex_property_path(vadas, &path("meta"))
        .unwrap();
    assert!(reads_the_base(&layer, &base, "2"));
    // Labels: added and removed again.
    layer.add_vertex_label(marko, "x".into()).unwrap();
    layer.remove_vertex_label(marko, "x").unwrap();
    assert!(reads_the_base(&layer, &base, "1"));
    // A base label removed and added back is last no more: same set, same order -> clean.
    layer.remove_vertex_label(marko, "person").unwrap();
    layer.add_vertex_label(marko, "person".into()).unwrap();
    assert!(reads_the_base(&layer, &base, "1"));
    // A key dropped and set again moves to the end: not the base's order, the image stays,
    // exactly as a `TraversalGraph` reorders it.
    layer.drop_vertex_property("name", marko).unwrap();
    layer
        .set_vertex_property(marko, "name".into(), "marko".into())
        .unwrap();
    assert!(!reads_the_base(&layer, &base, "1"));
    let keys: Vec<String> = layer
        .vertex_properties(marko)
        .unwrap()
        .unwrap()
        .keys()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(keys, ["age", "name"]);
    // An edge: relabeled and back, a property set and back.
    let knows = edge_of(&layer, "0");
    layer.set_edge_label(knows, "met".into()).unwrap();
    layer
        .set_edge_property(knows, "weight".into(), 0.1.into())
        .unwrap();
    layer.set_edge_label(knows, "knows".into()).unwrap();
    assert!(!edge_reads_the_base(&layer, &base, "0"));
    layer
        .set_edge_property(knows, "weight".into(), 0.5.into())
        .unwrap();
    assert!(edge_reads_the_base(&layer, &base, "0"));
    // A vertex with an added edge returns to clean too and keeps its edge.
    let n = layer
        .insert_vertex(None, vec!["n".into()], PropertyMap::default())
        .unwrap();
    layer
        .insert_edge(None, None, vadas, n, PropertyMap::default())
        .unwrap();
    layer
        .set_vertex_property(vadas, "age".into(), 1.into())
        .unwrap();
    layer
        .set_vertex_property(vadas, "age".into(), 27.into())
        .unwrap();
    assert!(reads_the_base(&layer, &base, "2"));
    assert_eq!(layer.out_edges(vadas).unwrap().count(), 1);
    // The by-label lookups follow every return.
    let mut reference = TraversalGraph::tinkerpop_modern();
    let r_marko = vertex_of(&reference, "1");
    reference.drop_vertex_property("name", r_marko).unwrap();
    reference
        .set_vertex_property(r_marko, "name".into(), "marko".into())
        .unwrap();
    let rn = reference
        .insert_vertex(None, vec!["n".into()], PropertyMap::default())
        .unwrap();
    let r_vadas = vertex_of(&reference, "2");
    reference
        .insert_edge(None, None, r_vadas, rn, PropertyMap::default())
        .unwrap();
    assert_eq!(
        state(&layer).with_sorted_buckets(),
        state(&reference).with_sorted_buckets()
    );
}

/// The base's schema applies to writes on base elements exactly as in a `TraversalGraph`
/// holding the same data: coercion, type and required violations, closed-mode labels and
/// topology, path faults; a failed write leaves no trace.
#[test]
fn the_base_schema_applies_to_writes_on_base_elements() {
    const SCHEMA: &str = r#"{
      "mode": "closed",
      "vertices": {
        "person": {"schema": {"type": "object", "properties": {
          "name": {"type": "string"},
          "score": {"type": "number"},
          "meta": {"type": "object", "properties": {"n": {"type": "integer"}}}
        }, "required": ["name"]}},
        "software": {"schema": {"type": "object", "properties": {"name": {"type": "string"}}}}
      },
      "edges": {
        "knows": {"schema": {"type": "object", "properties": {"w": {"type": "number"}}},
                  "connections": [{"from": "person", "to": "person"}]},
        "uses": {"connections": [{"from": "person", "to": "software"}]}
      }
    }"#;
    fn graph() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        graph
            .set_schema(GraphSchema::from_json(SCHEMA).unwrap())
            .unwrap();
        let p = graph
            .insert_vertex(
                Some("p".into()),
                vec!["person".into()],
                props(&[("name", "p".into())]),
            )
            .unwrap();
        let q = graph
            .insert_vertex(
                Some("q".into()),
                vec!["person".into()],
                props(&[("name", "q".into())]),
            )
            .unwrap();
        let w = graph
            .insert_vertex(
                Some("w".into()),
                vec!["software".into()],
                props(&[("name", "w".into())]),
            )
            .unwrap();
        graph
            .insert_edge(
                Some("k".into()),
                Some("knows".into()),
                p,
                q,
                PropertyMap::default(),
            )
            .unwrap();
        graph
            .insert_edge(
                Some("u".into()),
                Some("uses".into()),
                p,
                w,
                PropertyMap::default(),
            )
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
        let (p, q, w) = (vertex_of(s, "p"), vertex_of(s, "q"), vertex_of(s, "w"));
        let (k, u) = (edge_of(s, "k"), edge_of(s, "u"));
        let ok = |_| "ok".to_string();
        record(s.set_vertex_property(q, "score".into(), 2.into()).map(ok));
        record(s.set_vertex_property(q, "score".into(), "x".into()).map(ok));
        record(s.set_vertex_property(q, "other".into(), 2.into()).map(ok));
        record(s.drop_vertex_property("name", q).map(ok));
        record(
            s.set_vertex_property_path(p, &path("meta.n"), 1.5.into())
                .map(ok),
        );
        record(
            s.set_vertex_property_path(p, &path("meta.n"), 2.into())
                .map(ok),
        );
        record(
            s.set_vertex_property_path(p, &path("meta.zz"), 2.into())
                .map(ok),
        );
        record(
            s.set_vertex_property_path(p, &path("name.x"), 2.into())
                .map(ok),
        );
        record(s.remove_vertex_property_path(p, &path("name")).map(ok));
        record(s.add_vertex_label(w, "robot".into()).map(|b| b.to_string()));
        record(
            s.add_vertex_label(w, "person".into())
                .map(|b| b.to_string()),
        );
        record(s.remove_vertex_label(q, "person").map(|b| b.to_string()));
        record(s.set_edge_label(k, "uses".into()).map(|b| b.to_string()));
        record(s.set_edge_label(u, "likes".into()).map(|b| b.to_string()));
        record(s.set_edge_property(k, "w".into(), 3.into()).map(ok));
        record(s.set_edge_property(k, "w".into(), "x".into()).map(ok));
        record(s.set_edge_property(u, "w".into(), 1.into()).map(ok));
        (outcomes, state(s))
    }
    let on_layer = run(&mut LayeredGraph::over(Arc::new(graph())).unwrap());
    let on_reference = run(&mut graph());
    assert_eq!(on_layer, on_reference);
    let failures = on_layer.0.iter().filter(|o| *o != "ok").count();
    assert!(failures >= 8, "{:?}", on_layer.0);
    assert!(on_layer.0.iter().filter(|o| *o == "ok").count() >= 3);
}

/// A base whose values are stored coded (a top-level `dedup` rule), compressed (an lz4 rule)
/// and deduplicated (a nested-path `dedup` rule): the layer copies an element up with the
/// logical values, reads answer as on a `TraversalGraph` with the same rules and writes, and a
/// write of the stored value is still recognized as equal.
#[test]
fn writes_on_coded_and_compressed_base_values() {
    use graphersal::catalog::{Codec, CompressionDefinition};
    use graphersal::storage::ElementKind;
    fn long(seed: usize) -> String {
        (0..120)
            .map(|i| format!("word{} ", (seed * 7 + i) % 13))
            .collect()
    }
    fn graph() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        for i in 0..4 {
            let meta = ElementProperty::Object(Box::new(props(&[
                (
                    "file",
                    format!("src/some/rather/long/path/file{}.rs", i % 2).into(),
                ),
                ("n", (i as i64).into()),
            ])));
            graph
                .insert_vertex(
                    Some(format!("d{i}").into()),
                    vec!["doc".into()],
                    props(&[
                        ("kind", ["alpha", "beta"][i % 2].into()),
                        ("body", long(i).into()),
                        ("meta", meta),
                    ]),
                )
                .unwrap();
        }
        let d0 = graph.get_vertex_by_id("d0").unwrap().unwrap();
        let d1 = graph.get_vertex_by_id("d1").unwrap().unwrap();
        graph
            .insert_edge(
                Some("r".into()),
                Some("ref".into()),
                d0,
                d1,
                props(&[("site", "here".into())]),
            )
            .unwrap();
        let dedup = |element, label, key| {
            CompressionDefinition::new(element, label, key)
                .unwrap()
                .with_codec(Codec::Dedup)
        };
        let mut g = graph.traversal_mut();
        g.define_compression("kind", dedup(ElementKind::Vertex, "doc", "kind"))
            .unwrap();
        g.define_compression("file", dedup(ElementKind::Vertex, "doc", "meta.file"))
            .unwrap();
        g.define_compression("site", dedup(ElementKind::Edge, "ref", "site"))
            .unwrap();
        g.define_compression(
            "body",
            CompressionDefinition::new(ElementKind::Vertex, "doc", "body")
                .unwrap()
                .with_dictionary(false),
        )
        .unwrap();
        drop(g);
        graph
    }
    fn run<S: GraphStorage>(s: &mut S) -> Vec<State> {
        let mut states = Vec::new();
        let d = |s: &S, i: usize| vertex_of(s, &format!("d{i}"));
        let (d0, d1, d2, d3) = (d(s, 0), d(s, 1), d(s, 2), d(s, 3));
        let r = edge_of(s, "r");
        // Equal values: no change at all.
        s.set_vertex_property(d0, "kind".into(), "alpha".into())
            .unwrap();
        s.set_vertex_property(d0, "body".into(), long(0).into())
            .unwrap();
        s.set_edge_property(r, "site".into(), "here".into())
            .unwrap();
        states.push(state(s));
        s.set_vertex_property(d1, "extra".into(), 1.into()).unwrap();
        s.set_vertex_property_path(d2, &path("meta.n"), 9.into())
            .unwrap();
        s.drop_vertex_property("body", d3).unwrap();
        s.set_vertex_property(d0, "kind".into(), "gamma".into())
            .unwrap();
        s.set_edge_property(r, "w".into(), 1.into()).unwrap();
        states.push(state(s));
        s.remove_vertex_property_path(d2, &path("meta.file"))
            .unwrap();
        s.set_edge_property(r, "site".into(), "there".into())
            .unwrap();
        states.push(state(s));
        states
    }
    let base = Arc::new(graph());
    assert!(base.is_dedup_coded(ElementKind::Vertex, "d1", "kind"));
    let stored = |id: &str| {
        let node = base.get_vertex_by_id(id).unwrap().unwrap();
        base.vertex_properties(node).unwrap().unwrap().clone()
    };
    assert!(
        matches!(
            stored("d1").get("body"),
            Some(ElementProperty::Compressed(_))
        ),
        "the base stores the body compressed"
    );
    assert!(
        stored("d2")["meta"].holds_compressed(),
        "the base stores the nested file deduplicated"
    );
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let on_layer = run(&mut layer);
    let on_reference = run(&mut graph());
    for (step, (l, r)) in on_layer.iter().zip(&on_reference).enumerate() {
        assert_eq!(l, r, "step {step}");
    }
    // Equal writes made no image: d0 was read from the base until its kind changed; the
    // images hold logical values only.
    for id in ["d1", "d2", "d3"] {
        let node = v(&layer, id);
        let image = layer.vertex_properties(node).unwrap().unwrap();
        assert!(
            image.iter().all(|(_, value)| !value.holds_compressed()),
            "{id}: {image:?}"
        );
        for (key, value) in stored(id).iter() {
            if let Some(now) = image.get(key) {
                if key == "meta" {
                    continue;
                }
                assert_eq!(now, &value.to_logical(), "{id}.{key}");
            }
        }
    }
    let untouched = v(&layer, "d0");
    assert_eq!(
        layer.vertex_properties(untouched).unwrap().unwrap()["kind"],
        ElementProperty::from("gamma")
    );
    // A fresh layer: the equal writes alone leave d0 and the edge reading the base.
    let mut fresh = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let d0 = v(&fresh, "d0");
    let r = edge_of(&fresh, "r");
    fresh
        .set_vertex_property(d0, "kind".into(), "alpha".into())
        .unwrap();
    fresh
        .set_vertex_property(d0, "body".into(), long(0).into())
        .unwrap();
    fresh
        .set_edge_property(r, "site".into(), "here".into())
        .unwrap();
    assert!(reads_the_base(&fresh, &base, "d0"));
    assert!(edge_reads_the_base(&fresh, &base, "r"));
}

/// Plan S1.5 correction: a property index lookup never returns fewer elements than exist. While
/// the layer has added, removed or changed an element of the kind asked about, the lookup and
/// the count answer `None` (the caller scans); untouched, they are the base's.
#[test]
fn property_index_lookups_never_miss_layer_changes() {
    use graphersal::catalog::{Definition, PropertyIndexDefinition};
    use graphersal::storage::{ElementKind, PropertyIndexRange};
    let mut base = TraversalGraph::tinkerpop_modern();
    let mut g = base.traversal_mut();
    g.set_definition(Definition::property_index(
        "person_name",
        PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
    ))
    .unwrap();
    g.set_definition(Definition::property_index(
        "created_weight",
        PropertyIndexDefinition::new(ElementKind::Edge, "created", "weight"),
    ))
    .unwrap();
    drop(g);
    let base = Arc::new(base);
    let named = |name: &str| PropertyIndexRange::equal(name.into());
    let lookup = |layer: &LayeredGraph, name: &str| {
        layer
            .vertices_by_property_index(Some("person"), "name", &named(name))
            .unwrap()
            .map(|found| found.count())
    };
    let count = |layer: &LayeredGraph,
                 kind: ElementKind,
                 label: &str,
                 key: &str,
                 value: ElementProperty| {
        layer
            .count_by_property_index(
                kind,
                Some(label),
                key,
                None,
                &PropertyIndexRange::equal(value),
            )
            .unwrap()
    };
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    assert_eq!(
        lookup(&layer, "marko"),
        Some(1),
        "untouched: the base's index"
    );
    assert_eq!(
        count(
            &layer,
            ElementKind::Vertex,
            "person",
            "name",
            "marko".into()
        ),
        Some(1)
    );
    // An added edge alone leaves the vertex lookups to the base.
    let (marko, vadas) = (v(&layer, "1"), v(&layer, "2"));
    layer
        .insert_edge(None, Some("x".into()), marko, vadas, PropertyMap::default())
        .unwrap();
    assert_eq!(lookup(&layer, "marko"), Some(1));
    assert_eq!(
        count(&layer, ElementKind::Edge, "created", "weight", 0.4.into()),
        None,
        "an added edge: the edge counts scan"
    );
    layer
        .insert_vertex(
            None,
            vec!["person".into()],
            props(&[("name", "zed".into())]),
        )
        .unwrap();
    assert_eq!(lookup(&layer, "zed"), None);
    assert_eq!(
        count(&layer, ElementKind::Vertex, "person", "name", "zed".into()),
        None
    );
    // Through traversals, against a `TraversalGraph` with the same indexes and writes: an
    // added match, a renamed base vertex, a dropped one, a changed and a relabeled edge.
    fn changes<S: GraphStorage>(s: &mut S) {
        let (josh, peter) = (vertex_of(s, "4"), vertex_of(s, "6"));
        s.insert_vertex(
            None,
            vec!["person".into()],
            props(&[("name", "josh".into())]),
        )
        .unwrap();
        s.set_vertex_property(josh, "name".into(), "joshua".into())
            .unwrap();
        s.drop_vertex(peter).unwrap();
        let (lop, ripple) = (edge_of(s, "3"), edge_of(s, "4"));
        s.set_edge_property(lop, "weight".into(), 1.0.into())
            .unwrap();
        s.set_edge_label(ripple, "made".into()).unwrap();
    }
    fn reads<S: GraphStorage>(s: &S) -> Vec<ElementProperty> {
        let g = || GraphTraversalSource::new(s);
        let mut out = Vec::new();
        for name in ["josh", "joshua", "peter", "zed", "marko"] {
            out.extend(
                g().v(None)
                    .has_label("person")
                    .has("name", name)
                    .id()
                    .to_list()
                    .unwrap(),
            );
            out.extend(
                g().v(None)
                    .has_label("person")
                    .has("name", name)
                    .count()
                    .to_list()
                    .unwrap(),
            );
        }
        for weight in [0.4, 1.0, 0.2] {
            out.extend(
                g().e(None)
                    .has_label("created")
                    .has("weight", weight)
                    .id()
                    .to_list()
                    .unwrap(),
            );
            out.extend(
                g().e(None)
                    .has_label("created")
                    .has("weight", weight)
                    .count()
                    .to_list()
                    .unwrap(),
            );
        }
        out
    }
    let mut reference = TraversalGraph::tinkerpop_modern();
    let mut g = reference.traversal_mut();
    g.set_definition(Definition::property_index(
        "person_name",
        PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
    ))
    .unwrap();
    g.set_definition(Definition::property_index(
        "created_weight",
        PropertyIndexDefinition::new(ElementKind::Edge, "created", "weight"),
    ))
    .unwrap();
    drop(g);
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    changes(&mut layer);
    changes(&mut reference);
    let expected = reads(&reference);
    assert_eq!(reads(&layer), expected);
    assert!(
        expected.contains(&ElementProperty::from("7")),
        "the added josh is found: {expected:?}"
    );
}

/// Scripts that write to base elements (properties, `Cardinality.single`, jpath writes and
/// removals, label additions, edge relabels, property drops, `mergeV` matches) and then
/// traverse by label and property: the same result and the same graph state, label lookups
/// included, exactly.
#[cfg(feature = "script")]
const RHAI_BASE_WRITES: &[&str] = &[
    r#"g.V("1").property("age", 30).iterate(); g.V().has("age", gt(29)).values("name").toList()"#,
    r#"g.V("1").property("age", 29).iterate(); g.V("1").valueMap().next()"#,
    r#"g.V("1").property("age", 29.0).iterate(); [g.V("1").values("age").next(), g.V().has("age", 29).count().next()]"#,
    r#"g.V().hasLabel("person").property("seen", true).iterate(); [g.V().has("seen", true).values("name").toList(), g.V().hasLabel("person").valueMap().toList()]"#,
    r#"g.V("1").properties("age").drop().iterate(); g.V().hasLabel("person").values("age").toList()"#,
    r#"g.V("4").properties().drop().iterate(); [g.V("4").valueMap().next(), g.V().has("name").count().next()]"#,
    r#"g.E("0").property("weight", 0.7).iterate(); g.E().has("weight", gt(0.6)).id().toList()"#,
    r#"g.E().hasLabel("created").properties("weight").drop().iterate(); g.E().valueMap().toList()"#,
    r#"g.E("0").property("weight", 0.5).iterate(); g.E("0").valueMap().next()"#,
    r#"g.V("2").property(jpath("meta.tags"), ["a"]).property(jpath("meta.tags[1]"), "b").iterate(); g.V("2").values(jpath("meta.tags")).next()"#,
    r#"g.V("2").property(jpath("meta.tags"), ["a", "b"]).iterate(); g.V("2").remove_property(jpath("meta.tags[0]")).iterate(); g.V("2").valueMap().next()"#,
    r#"g.E("1").property(jpath("info.by"), "x").iterate(); g.E("1").remove_property(jpath("info")).iterate(); g.E("1").valueMap().next()"#,
    r#"g.V("1").property(Cardinality.single, "name", "marco").iterate(); g.V().has("name", "marco").id().toList()"#,
    r#"g.V("1").property("age", 30).property("age", 29).iterate(); g.V("1").valueMap().next()"#,
    r#"g.V("1").property("name", "x").iterate(); g.V("1").property("name", "marko").iterate(); g.V().has("name", "marko").outE().count().next()"#,
    r#"g.V("1").addE("self").to(__.V("1")).property("w", 1).iterate(); g.V("1").property("age", 40).iterate(); [g.V("1").bothE().count().next(), g.V("1").values("age").next()]"#,
    r#"g.V("4").property("age", 33).iterate(); g.V("4").drop().iterate(); [g.V().count().next(), g.statistics()]"#,
    r#"g.mergeV(#{"T.label": "person", name: "marko"}).option(Merge.onMatch, #{age: 50}).values("age").next()"#,
    r#"g.V("1").add_label("employee").iterate(); g.addV("employee").property("name", "e").iterate(); g.V("2").add_label("employee").iterate(); g.V().hasLabel("employee").values("name").toList()"#,
    r#"g.V("3").add_label("person").iterate(); [g.V().hasLabel("person").values("name").toList(), g.V("3").labels().next(), g.statistics()]"#,
    r#"g.E("0").set_label("met").iterate(); [g.E().hasLabel("met").id().toList(), g.E().hasLabel("knows").id().toList(), g.V("1").out("met").values("name").toList()]"#,
    r#"g.E("0").set_label("created").iterate(); [g.E().hasLabel("created").id().toList(), g.V("1").out("created").values("name").toList()]"#,
    r#"g.V("1").property("age", 30).iterate(); g.V("1").property(Cardinality.list, "age", 31).iterate()"#,
    r#"g.V("1").property("age", "old").property("x", 1).iterate(); g.V("1").valueMap().next()"#,
];

/// Scripts that remove label memberships of base elements (labels dropped, edges relabeled
/// away and back): compared exactly first; where only the order differs (a `TraversalGraph`
/// moves a bucket's last member into a removed one's place, the layer keeps the base bucket's
/// order: plan P4) the result and the state are compared sorted. Each result here is
/// order-independent or sorted.
#[cfg(feature = "script")]
const RHAI_BASE_LABEL_REMOVALS: &[&str] = &[
    r#"g.V("1").drop_label("person").iterate(); [g.V().hasLabel("person").values("name").order().toList(), g.V("1").labels().next()]"#,
    r#"g.V("2").drop_label("person").add_label("person").iterate(); g.V().hasLabel("person").id().order().toList()"#,
    r#"g.V("1").add_label("x").iterate(); g.V("1").drop_label("x").iterate(); [g.V().hasLabel("x").count().next(), g.statistics()]"#,
    r#"g.E("0").set_label("met").iterate(); g.E("0").set_label("knows").iterate(); g.E().hasLabel("knows").id().order().toList()"#,
    r#"g.V().hasLabel("software").drop_label("software").add_label("tool").iterate(); [g.V().hasLabel("tool").values("name").order().toList(), g.V().hasLabel("software").count().next(), g.V("1").out().labels().toList()]"#,
    r#"g.E().hasLabel("knows").set_label("met").iterate(); g.addE("met").from(__.V("6")).to(__.V("1")).iterate(); g.E().hasLabel("met").id().order().toList()"#,
];

#[cfg(feature = "script")]
const LABEL_STATE_SCRIPT: &str = r#"[
    g.V().elementMap().toList(),
    g.E().elementMap().toList(),
    g.V().project("v", "labels", "out", "in").by(T.id).by(__.labels()).by(__.outE().id().fold()).by(__.inE().id().fold()).toList(),
    g.V().hasLabel("person").id().toList(),
    g.V().hasLabel("software").id().toList(),
    g.V().hasLabel("employee").id().toList(),
    g.V().hasLabel("tool").id().toList(),
    g.E().hasLabel("knows").id().toList(),
    g.E().hasLabel("created").id().toList(),
    g.E().hasLabel("met").id().toList(),
    g.statistics()
]"#;

#[cfg(feature = "script")]
const SORTED_LABEL_STATE_SCRIPT: &str = r#"[
    g.V().order().by(T.id).elementMap().toList(),
    g.E().order().by(T.id).elementMap().toList(),
    g.V().order().by(T.id).project("v", "labels", "out", "in").by(T.id).by(__.labels()).by(__.outE().id().fold()).by(__.inE().id().fold()).toList(),
    g.V().hasLabel("person").id().order().toList(),
    g.V().hasLabel("software").id().order().toList(),
    g.V().hasLabel("employee").id().order().toList(),
    g.V().hasLabel("tool").id().order().toList(),
    g.E().hasLabel("knows").id().order().toList(),
    g.E().hasLabel("created").id().order().toList(),
    g.E().hasLabel("met").id().order().toList(),
    g.statistics()
]"#;

#[cfg(feature = "script")]
#[test]
fn rhai_base_write_scripts_match_the_plain_graph() {
    let mut differences = Vec::new();
    let mut order_only = Vec::new();
    for (script, may_reorder) in RHAI_BASE_WRITES
        .iter()
        .map(|s| (s, false))
        .chain(RHAI_BASE_LABEL_REMOVALS.iter().map(|s| (s, true)))
    {
        let plain = plain_graph();
        let layered = layered_graph();
        let expected = render(Arc::clone(&plain), script);
        let actual = render(Arc::clone(&layered), script);
        let expected_state = render(Arc::clone(&plain), LABEL_STATE_SCRIPT);
        let actual_state = render(Arc::clone(&layered), LABEL_STATE_SCRIPT);
        if expected == actual && expected_state == actual_state {
            continue;
        }
        if may_reorder
            && expected == actual
            && render(plain, SORTED_LABEL_STATE_SCRIPT)
                == render(layered, SORTED_LABEL_STATE_SCRIPT)
        {
            order_only.push(*script);
            continue;
        }
        differences.push(format!(
            "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}\n  state TraversalGraph: {expected_state:?}\n  state LayeredGraph:   {actual_state:?}"
        ));
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    println!(
        "order-only differences of label lookups (P4): {}\n{}",
        order_only.len(),
        order_only.join("\n")
    );
}
