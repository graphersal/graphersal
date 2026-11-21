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
//! bases, label lookups, property-index lookups and a Rhai differential. The layer's own
//! schema and catalog (S1.7): copies independent of the base, `set_schema` with the errors of a
//! `TraversalGraph`, open-mode extension, saved queries, refused kinds, the refusals of a base
//! (plan decision P5) and a Rhai differential. Units (S1.8): a rollback of every write kind
//! restores the prior state, savepoints, commits and the commit count, units of their own for
//! schema and catalog writes, transactions and dry runs, and Rhai differentials of failing
//! traversals and whole-script units. Change capture (S1.9): the change set of every write
//! kind equals a `TraversalGraph`'s for the same writes on the same data (committed and as a
//! dry run), change sets apply across both, vetoes and rollbacks reach the hooks with their
//! reason, the commit clock, and Rhai differentials of dry runs and captured commits.
#![allow(clippy::type_complexity)] // state tuples of the differential

#[cfg(feature = "script")]
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use graphersal::prelude::*;
#[cfg(feature = "script")]
use graphersal::storage::IntoAnyGraph;
use graphersal::storage::{
    Graph, GraphStatistics, GraphStorage, JsonPath, LayeredEdge, LayeredGraph, LayeredVertex,
    RollbackReason,
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
    use graphersal::catalog::{CompressionDefinition, Definition, DefinitionKind};
    use graphersal::storage::ElementKind;
    let mut layer = modern_layer();
    let marko = v(&layer, "1");
    let unsupported = |result: Result<(), GraphError>| {
        assert!(
            matches!(result, Err(GraphError::Unsupported { .. })),
            "{result:?}"
        );
    };
    // The catalog kinds that need storage machinery (stage 4).
    unsupported(layer.set_definition(Definition::compression(
        "n",
        CompressionDefinition::new(ElementKind::Vertex, "person", "name").unwrap(),
    )));
    unsupported(
        layer
            .remove_definition(DefinitionKind::MODEL, "m")
            .map(|_| ()),
    );
    assert_eq!(layer.definitions().unwrap().count(), 0);
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
    assert!(caps.writable && caps.atomic && caps.change_capture);
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
    let actual = render(
        layered_graph(),
        r#"g.define_compression(#{name: "n", label: "person", path: "name"})"#,
    );
    let err = actual.unwrap_err();
    assert!(
        err.contains("does not support compression rules on a layered graph yet"),
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
        {
            let node = marko;
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

// ------------------------------------------------------------------------------------------
// The layer's own schema and catalog (S1.7)
// ------------------------------------------------------------------------------------------

/// The product schema of the design (mode `open`): `person` and `software` declared.
const PRODUCT_SCHEMA: &str = r#"{
  "mode": "open",
  "vertices": {
    "person": {"schema": {"type": "object", "properties": {
      "name": {"type": "string"}, "age": {"type": "integer"}}}},
    "software": {"schema": {"type": "object", "properties": {"name": {"type": "string"}}}}
  }
}"#;

/// A project's extension of [`PRODUCT_SCHEMA`]: a new label with a default, a date and a
/// number, a new person property with a default and one with a date, an edge label with a
/// number.
const PROJECT_SCHEMA: &str = r#"{
  "mode": "open",
  "vertices": {
    "person": {"schema": {"type": "object", "properties": {
      "name": {"type": "string"}, "age": {"type": "integer"},
      "team": {"type": "string", "default": "core"},
      "since": {"type": "string", "format": "date"}}}},
    "software": {"schema": {"type": "object", "properties": {"name": {"type": "string"}}}},
    "project": {"schema": {"type": "object", "properties": {
      "name": {"type": "string"},
      "status": {"type": "string", "default": "active"},
      "started": {"type": "string", "format": "date"},
      "budget": {"type": "number"}}, "required": ["name"]}}
  },
  "edges": {
    "works_on": {"schema": {"type": "object", "properties": {"weight": {"type": "number"}}}}
  }
}"#;

/// Modern with [`PRODUCT_SCHEMA`] and the saved query `people`: the product a layer extends.
fn product_base() -> TraversalGraph {
    let mut base = TraversalGraph::tinkerpop_modern();
    base.set_schema(GraphSchema::from_json(PRODUCT_SCHEMA).unwrap())
        .unwrap();
    base.traversal_mut()
        .define_query(
            "people",
            graphersal::catalog::QueryDefinition::new(
                r#"fn people() { g.v().has_label("person") }"#,
            ),
        )
        .unwrap();
    base
}

fn definitions_of<S: GraphStorage>(s: &S) -> Vec<graphersal::catalog::Definition> {
    s.definitions().unwrap().cloned().collect()
}

/// The layer starts with copies of the base's schema and catalog; changing them changes only
/// the layer: the base keeps its own, and a second layer over the same base sees the base's.
#[test]
fn the_layer_schema_and_catalog_are_its_own_copies() {
    use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
    let base = Arc::new(product_base());
    let base_schema = GraphStorage::get_schema(&*base).unwrap();
    let base_catalog = definitions_of(&*base);
    assert!(base_schema.is_some());
    assert_eq!(base_catalog.len(), 1);

    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    assert_eq!(layer.get_schema().unwrap(), base_schema);
    assert_eq!(definitions_of(&layer), base_catalog);

    let project = GraphSchema::from_json(PROJECT_SCHEMA).unwrap();
    layer.set_schema(project.clone()).unwrap();
    let mut reference = product_base();
    reference.set_schema(project).unwrap();
    assert_eq!(
        layer.get_schema().unwrap(),
        GraphStorage::get_schema(&reference).unwrap()
    );
    assert!(
        layer
            .declared_property_schema(
                graphersal::storage::GraphNodeType::Vertex,
                &["project".into()],
                "budget"
            )
            .unwrap()
            .is_some()
    );
    assert!(
        GraphStorage::declared_property_schema(
            &*base,
            graphersal::storage::GraphNodeType::Vertex,
            &["project".into()],
            "budget"
        )
        .unwrap()
        .is_none()
    );

    layer
        .set_definition(Definition::query(
            "mine",
            QueryDefinition::new("fn mine() { g.v() }"),
        ))
        .unwrap();
    assert!(
        layer
            .remove_definition(DefinitionKind::QUERY, "people")
            .unwrap()
    );
    assert!(
        !layer
            .remove_definition(DefinitionKind::QUERY, "people")
            .unwrap()
    );
    let names: Vec<String> = definitions_of(&layer)
        .iter()
        .map(|d| d.name().to_string())
        .collect();
    assert_eq!(names, ["mine"]);
    assert!(
        layer
            .definition(DefinitionKind::QUERY, "mine")
            .unwrap()
            .is_some()
    );

    // The base and a second layer still have the product's schema and catalog.
    assert_eq!(GraphStorage::get_schema(&*base).unwrap(), base_schema);
    assert_eq!(definitions_of(&*base), base_catalog);
    let second = LayeredGraph::over(Arc::clone(&base)).unwrap();
    assert_eq!(second.get_schema().unwrap(), base_schema);
    assert_eq!(definitions_of(&second), base_catalog);

    // Removing the schema (the empty schema) removes only the layer's.
    layer.set_schema(GraphSchema::new()).unwrap();
    assert_eq!(layer.get_schema().unwrap(), None);
    assert_eq!(GraphStorage::get_schema(&*base).unwrap(), base_schema);
}

/// The layer's catalog behaves as a `TraversalGraph`'s for the kinds it takes: an equal
/// definition again is no change, a malformed one is refused with the same error, a list with
/// a refused definition stores none of them (a `TraversalGraph` makes the list one unit).
#[test]
fn catalog_writes_match_a_traversal_graph() {
    use graphersal::catalog::{Definition, DefinitionFlags, DefinitionKind, QueryDefinition};
    fn run<S: GraphStorage>(s: &mut S) -> (Vec<String>, Vec<Definition>) {
        let mut outcomes = Vec::new();
        let mut record = |r: Result<String, GraphError>| {
            outcomes.push(match r {
                Ok(v) => v,
                Err(e) => format!("{e:?}"),
            })
        };
        let q = Definition::query("q", QueryDefinition::new("fn q() { g.v() }"));
        record(s.set_definition(q.clone()).map(|_| "ok".into()));
        record(s.set_definition(q).map(|_| "ok".into()));
        record(
            s.set_definition(Definition::query(
                "bad name!",
                QueryDefinition::new("fn x() { 1 }"),
            ))
            .map(|_| "ok".into()),
        );
        record(
            s.set_definitions(vec![
                Definition::query("a", QueryDefinition::new("fn a() { 1 }")),
                Definition::query("", QueryDefinition::new("fn b() { 1 }")),
                Definition::query("c", QueryDefinition::new("fn c() { 1 }")),
            ])
            .map(|_| "ok".into()),
        );
        let host = DefinitionKind::host(200).unwrap();
        record(
            s.set_definition(Definition::opaque(
                host,
                DefinitionFlags::NONE,
                "h",
                vec![1, 2, 3],
            ))
            .map(|_| "ok".into()),
        );
        record(
            s.remove_definition(DefinitionKind::QUERY, "q")
                .map(|b| b.to_string()),
        );
        record(
            s.remove_definitions(&[(DefinitionKind::QUERY, "a"), (host, "h"), (host, "none")])
                .map(|n| n.to_string()),
        );
        (outcomes, s.definitions().unwrap().cloned().collect())
    }
    let on_layer = run(&mut modern_layer());
    let on_reference = run(&mut TraversalGraph::tinkerpop_modern());
    assert_eq!(on_layer, on_reference);
    assert!(
        on_layer.0.iter().any(|o| o.contains("Definition")),
        "{:?}",
        on_layer.0
    );
}

/// `set_schema` checks the layer's current state (base elements, images of changed ones,
/// added elements) and fails exactly as a `TraversalGraph` with the same data and the same
/// schema: open and closed mode, violations in the base only, in the delta only, in images,
/// a mode change (full scan) and a declaration change (changed labels only), mode `none`.
#[test]
fn set_schema_errors_match_a_traversal_graph() {
    const AGE_REQUIRED: &str = r#"{"mode": "open", "vertices": {"person": {"schema":
        {"type": "object", "properties": {"age": {"type": "integer"}}, "required": ["age"]}}}}"#;
    const EMAIL_REQUIRED: &str = r#"{"mode": "open", "vertices": {"person": {"schema":
        {"type": "object", "required": ["email"]}}}}"#;
    const EMAIL_REQUIRED_NONE: &str = r#"{"mode": "none", "vertices": {"person": {"schema":
        {"type": "object", "required": ["email"]}}}}"#;
    const CLOSED: &str = r#"{"mode": "closed",
      "vertices": {
        "person": {"schema": {"type": "object", "additionalProperties": true}},
        "software": {"schema": {"type": "object", "additionalProperties": true}}},
      "edges": {
        "knows": {"schema": {"type": "object", "additionalProperties": true},
          "connections": [{"from": "person", "to": "person"}]},
        "created": {"schema": {"type": "object", "additionalProperties": true},
          "connections": [{"from": "person", "to": "software"}]}}}"#;
    const CLOSED_WITHOUT_CREATED: &str = r#"{"mode": "closed",
      "vertices": {
        "person": {"schema": {"type": "object", "additionalProperties": true}},
        "software": {"schema": {"type": "object", "additionalProperties": true}},
        "robot": {"schema": {"type": "object", "additionalProperties": true}}},
      "edges": {
        "knows": {"schema": {"type": "object", "additionalProperties": true},
          "connections": [{"from": "person", "to": "person"}]},
        "uses": {"connections": [{"from": "*", "to": "*"}]}}}"#;
    fn run<S: GraphStorage>(s: &mut S) -> (Vec<String>, Option<GraphSchema>, State) {
        let mut outcomes = Vec::new();
        let mut set = |s: &mut S, json: &str| {
            outcomes.push(match s.set_schema(GraphSchema::from_json(json).unwrap()) {
                Ok(()) => "ok".to_string(),
                Err(e) => format!("{e:?}"),
            });
        };
        let ann = s
            .insert_vertex(
                Some("ann".into()),
                vec!["person".into()],
                props(&[("name", "ann".into())]),
            )
            .unwrap();
        let marko = vertex_of(s, "1");
        s.set_vertex_property(marko, "age".into(), "old".into())
            .unwrap();
        // Delta (ann without age) and image (marko's age a string).
        set(s, AGE_REQUIRED);
        // Base and delta (no person has an email).
        set(s, EMAIL_REQUIRED);
        set(s, EMAIL_REQUIRED_NONE);
        // A mode change from `none`: everything is checked; the base satisfies CLOSED.
        set(s, CLOSED);
        set(s, r#"{"mode": "none"}"#);
        let robot = s
            .insert_vertex(
                Some("r".into()),
                vec!["robot".into()],
                PropertyMap::default(),
            )
            .unwrap();
        let lop = vertex_of(s, "3");
        let uses = s
            .insert_edge(
                Some("u".into()),
                Some("uses".into()),
                ann,
                lop,
                PropertyMap::default(),
            )
            .unwrap();
        // A closed schema refusing the delta only (robot, uses).
        set(s, CLOSED);
        // A closed schema refusing the base only (created) after the delta was allowed.
        set(s, CLOSED_WITHOUT_CREATED);
        s.drop_edge(uses).unwrap();
        s.drop_vertex(robot).unwrap();
        set(s, CLOSED);
        // A declaration change in the same mode: only person is checked again.
        set(
            s,
            &CLOSED.replace(
                r#""person": {"schema": {"type": "object", "additionalProperties": true}}"#,
                r#""person": {"schema": {"type": "object", "additionalProperties": true, "required": ["name"]}}"#,
            ),
        );
        // Writes now follow the layer's closed schema.
        outcomes.push(
            match s.insert_vertex(None, vec!["robot".into()], PropertyMap::default()) {
                Ok(_) => "ok".into(),
                Err(e) => format!("{e:?}"),
            },
        );
        outcomes.push(match s.drop_vertex_property("name", ann) {
            Ok(()) => "ok".into(),
            Err(e) => format!("{e:?}"),
        });
        (outcomes, s.get_schema().unwrap(), state(s))
    }
    let on_layer = run(&mut modern_layer());
    let on_reference = run(&mut TraversalGraph::tinkerpop_modern());
    assert_eq!(on_layer, on_reference);
    let outcomes = &on_layer.0;
    let refused = outcomes
        .iter()
        .filter(|o| o.contains("ExistingDataViolatesSchema"))
        .count();
    assert_eq!(refused, 4, "{outcomes:#?}");
    assert_eq!(
        outcomes.iter().filter(|o| *o == "ok").count(),
        5,
        "{outcomes:#?}"
    );
}

/// The product schema in mode `open`, extended by the project on the layer: creates get the
/// layer's defaults, writes (also to base elements) the layer's coercion and validation,
/// exactly as on a `TraversalGraph` that got the same schema; the base keeps the product's.
#[test]
fn open_mode_extension_then_writes_follow_the_layer_schema() {
    fn run<S: GraphStorage>(s: &mut S) -> (Vec<String>, State) {
        let mut outcomes = Vec::new();
        let mut record = |r: Result<String, GraphError>| {
            outcomes.push(match r {
                Ok(v) => v,
                Err(e) => format!("{e:?}"),
            })
        };
        // Under the product schema the project label is free: no default, no coercion.
        let early = s
            .add_vertex(
                Some("early".into()),
                vec!["project".into()],
                props(&[("started", "2024-01-01".into())]),
            )
            .unwrap();
        s.drop_vertex(early).unwrap();
        s.set_schema(GraphSchema::from_json(PROJECT_SCHEMA).unwrap())
            .unwrap();
        let p1 = s
            .add_vertex(
                Some("p1".into()),
                vec!["project".into()],
                props(&[("name", "p1".into()), ("started", "2024-03-02".into())]),
            )
            .unwrap();
        record(
            s.add_vertex(
                None,
                vec!["person".into()],
                props(&[("name", "zoe".into())]),
            )
            .map(|_| "ok".into()),
        );
        record(
            s.add_vertex(
                None,
                vec!["project".into()],
                props(&[("name", "p2".into()), ("budget", "lots".into())]),
            )
            .map(|_| "ok".into()),
        );
        record(
            s.add_vertex(None, vec!["project".into()], PropertyMap::default())
                .map(|_| "ok".into()),
        );
        record(
            s.set_vertex_property(p1, "budget".into(), 10.into())
                .map(|_| "ok".into()),
        );
        let marko = vertex_of(s, "1");
        record(
            s.set_vertex_property(marko, "since".into(), "2020-01-01".into())
                .map(|_| "ok".into()),
        );
        record(
            s.set_vertex_property(marko, "since".into(), "yesterday".into())
                .map(|_| "ok".into()),
        );
        record(
            s.add_edge(
                None,
                Some("works_on".into()),
                marko,
                p1,
                props(&[("weight", 1.into())]),
            )
            .map(|_| "ok".into()),
        );
        record(
            s.add_edge(
                None,
                Some("works_on".into()),
                marko,
                p1,
                props(&[("weight", "heavy".into())]),
            )
            .map(|_| "ok".into()),
        );
        (outcomes, state(s))
    }
    let base = Arc::new(product_base());
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let on_layer = run(&mut layer);
    let on_reference = run(&mut product_base());
    assert_eq!(on_layer, on_reference);
    assert_eq!(
        on_layer.0.iter().filter(|o| *o == "ok").count(),
        4,
        "{:?}",
        on_layer.0
    );
    // Defaults and coercion as stored in the layer.
    let p1 = v(&layer, "p1");
    let p1 = layer.vertex_properties(p1).unwrap().unwrap();
    assert_eq!(p1.get("status"), Some(&"active".into()));
    assert!(
        !matches!(p1.get("started"), Some(ElementProperty::String(_))),
        "{:?}",
        p1.get("started")
    );
    assert!(
        p1.get("budget") == Some(&ElementProperty::from(10.0_f64)),
        "{:?}",
        p1.get("budget")
    );
    let marko = layer.vertex_properties(v(&layer, "1")).unwrap().unwrap();
    assert!(
        !matches!(marko.get("since"), Some(ElementProperty::String(_))),
        "{:?}",
        marko.get("since")
    );
    // The base is untouched: its schema, and marko without `since`.
    assert_eq!(
        GraphStorage::get_schema(&*base).unwrap(),
        GraphStorage::get_schema(&product_base()).unwrap()
    );
    let base_marko = base.get_vertex_by_id("1").unwrap().unwrap();
    assert!(
        GraphStorage::vertex_properties(&*base, base_marko)
            .unwrap()
            .unwrap()
            .get("since")
            .is_none()
    );
}

/// Plan decision P5: a base with a unique property index or with vector rules cannot carry a
/// layer yet; a base with a plain index or a compression rule can. On a layer, a schema with
/// vector rules and the catalog kinds needing storage machinery are refused, naming the
/// limitation, and change nothing.
#[test]
fn p5_refusals_and_refused_catalog_kinds() {
    use graphersal::catalog::{
        CompressionDefinition, Definition, DefinitionKind, PropertyIndexDefinition,
    };
    use graphersal::storage::ElementKind;
    const RULE_SCHEMA: &str = r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
        "text": {"type": "string"},
        "emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "text"}}}}}}"#;
    let refusal = |err: GraphError, needle: &str| {
        assert!(matches!(err, GraphError::Unsupported { .. }), "{err:?}");
        let text = err.to_string();
        assert!(
            text.contains(needle) && text.contains("on a layered graph yet"),
            "{text}"
        );
        println!("{text}");
    };

    // A unique index in the base.
    let mut base = TraversalGraph::tinkerpop_modern();
    base.traversal_mut()
        .create_index(
            "person_name",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name").with_unique(true),
        )
        .unwrap();
    refusal(
        LayeredGraph::over(Arc::new(base)).unwrap_err(),
        "unique property indexes",
    );

    // Vector rules in the base's schema.
    let mut base = TraversalGraph::new();
    base.traversal_mut()
        .set_schema(GraphSchema::from_json(RULE_SCHEMA).unwrap())
        .unwrap();
    refusal(
        LayeredGraph::over(Arc::new(base)).unwrap_err(),
        "vector rules",
    );

    // A plain index and a compression rule are fine; the layer cannot change them.
    let mut base = TraversalGraph::tinkerpop_modern();
    base.traversal_mut()
        .create_index(
            "person_age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        )
        .unwrap();
    base.traversal_mut()
        .define_compression(
            "names",
            CompressionDefinition::new(ElementKind::Vertex, "person", "name").unwrap(),
        )
        .unwrap();
    let base = Arc::new(base);
    let mut layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let before = definitions_of(&layer);
    assert_eq!(before, definitions_of(&*base));
    refusal(
        layer
            .remove_definition(DefinitionKind::PROPERTY_INDEX, "person_age")
            .unwrap_err(),
        "property index definitions",
    );
    refusal(
        layer
            .remove_definition(DefinitionKind::COMPRESSION, "names")
            .unwrap_err(),
        "compression rules",
    );
    refusal(
        layer
            .set_definition(Definition::property_index(
                "person_email",
                PropertyIndexDefinition::new(ElementKind::Vertex, "person", "email")
                    .with_unique(true),
            ))
            .unwrap_err(),
        "property index definitions",
    );
    refusal(
        layer
            .remove_definition(DefinitionKind::MODEL, "any")
            .unwrap_err(),
        "embedding model definitions",
    );
    // A list with a refused definition stores none of them.
    refusal(
        layer
            .set_definitions(vec![
                Definition::query(
                    "first",
                    graphersal::catalog::QueryDefinition::new("fn first() { 1 }"),
                ),
                Definition::compression(
                    "ages",
                    CompressionDefinition::new(ElementKind::Vertex, "person", "lang").unwrap(),
                ),
            ])
            .unwrap_err(),
        "compression rules",
    );
    assert_eq!(definitions_of(&layer), before);

    // A schema with vector rules, directly and through the traversal source.
    let schema_before = layer.get_schema().unwrap();
    refusal(
        layer
            .set_schema(GraphSchema::from_json(RULE_SCHEMA).unwrap())
            .unwrap_err(),
        "vector rules",
    );
    let through_source = GraphTraversalSource::new(&mut layer)
        .set_schema(GraphSchema::from_json(RULE_SCHEMA).unwrap())
        .unwrap_err();
    assert!(
        through_source.to_string().contains("vector rules"),
        "{through_source}"
    );
    println!("{through_source}");
    assert_eq!(layer.get_schema().unwrap(), schema_before);
    // A plain vector property (no rule) is an ordinary declaration.
    layer
        .set_schema(
            GraphSchema::from_json(
                r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
                    "emb": {"type": "array", "format": "vector", "dimension": 3}}}}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
}

/// Saved queries in Rhai: the base's are callable on the layer, the layer's own run there and
/// stay out of the base and other layers.
#[cfg(feature = "script")]
#[test]
fn rhai_saved_queries_of_base_and_layer_run_on_the_layer() {
    let base = Arc::new(product_base());
    let layered = Arc::new(Graph::new(LayeredGraph::over(Arc::clone(&base)).unwrap()));
    assert_eq!(
        render(Arc::clone(&layered), r#"g.query("people").count().next()"#),
        Ok("4".to_string())
    );
    assert_eq!(
        render(
            Arc::clone(&layered),
            r#"g.define_query(#{name: "older", params: #{min: #{schema: #{type: "integer"}, default: 30}}, body: "fn older(min) { g.v().has(\"age\", P.gt(min)) }"}); g.addV("person").property("name", "old").property("age", 99).iterate(); g.query("older", #{min: 31}).values("name").order().toList()"#
        ),
        Ok(r#"["josh", "old", "peter"]"#.to_string())
    );
    assert!(
        GraphStorage::definition(&*base, graphersal::catalog::DefinitionKind::QUERY, "older")
            .unwrap()
            .is_none()
    );
    let second = Arc::new(Graph::new(LayeredGraph::over(base).unwrap()));
    assert!(render(second, r#"g.query("older").count().next()"#).is_err());
}

/// Schema and catalog scripts: each on a fresh plain graph and a fresh layer over the same
/// product base; the result and the state (data, schema, saved queries, jobs) must match.
#[cfg(feature = "script")]
const RHAI_SCHEMA_SCRIPTS: &[&str] = &[
    r#"g.set_schema(g.infer_schema()); g.get_schema()"#,
    r#"g.set_schema(#{mode: "closed", vertices: #{person: #{schema: #{type: "object"}}}})"#,
    r#"g.set_schema(#{mode: "closed", vertices: #{person: #{schema: #{type: "object", additionalProperties: true}}, software: #{schema: #{type: "object", additionalProperties: true}}}, edges: #{knows: #{connections: [#{from: "person", to: "person"}]}, created: #{connections: [#{from: "person", to: "software"}]}}}); g.addV("robot").iterate()"#,
    r#"g.set_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object", required: ["email"]}}}})"#,
    r#"g.addV("person").property("name", "ann").iterate(); g.set_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object", properties: #{age: #{type: "integer"}}, required: ["age"]}}}})"#,
    r#"g.V("1").property("age", "old").iterate(); g.set_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object", properties: #{age: #{type: "integer"}}}}}})"#,
    r#"g.patch_schema(#{vertices: #{project: #{schema: #{type: "object", properties: #{status: #{type: "string", default: "active"}, started: #{type: "string", format: "date"}}}}}}); g.addV("project").property("started", "2024-03-02").iterate(); g.V().hasLabel("project").valueMap().next()"#,
    r#"g.patch_schema(#{vertices: #{person: #{schema: #{properties: #{team: #{type: "string", default: "core"}}}}}}); g.addV("person").property("name", "zoe").iterate(); g.V().has("name", "zoe").values("team").next()"#,
    r#"g.patch_schema(#{mode: "closed"})"#,
    r#"g.set_schema(#{}); g.get_schema()"#,
    r#"g.query("people").values("name").order().toList()"#,
    r#"g.define_query(#{name: "n", body: "fn n() { g.v().count() }"}); g.query("n").next()"#,
    r#"g.drop_query("people")"#,
    r#"g.define_query(#{name: "n", folder: "a/b", body: "fn n() { 1 }"}); g.move_query("n", "archive"); g.get_query("n")["folder"]"#,
    r#"g.define_job(#{name: "nightly", schedule: "0 3 * * *", action: "checkpoint"}); g.jobs().map(|j| j.name)"#,
    r#"g.define_job(#{name: "nightly", schedule: "0 3 * * *", action: "checkpoint"}); g.drop_job("nightly")"#,
    r#"g.define_query(#{name: "add", body: "fn add() { g.add_v(\"person\").property(\"name\", \"q\") }"}); g.query("add").iterate(); g.V().has("name", "q").count().next()"#,
];

#[cfg(feature = "script")]
const SCHEMA_STATE_SCRIPT: &str = r#"[
    g.V().elementMap().toList(),
    g.E().elementMap().toList(),
    g.statistics(),
    g.get_schema(),
    g.queries().map(|q| [q.name, q.folder]),
    g.jobs().map(|j| j.name)
]"#;

#[cfg(feature = "script")]
#[test]
fn rhai_schema_and_catalog_scripts_match_the_plain_graph() {
    let mut differences = Vec::new();
    let mut failing = 0;
    for script in RHAI_SCHEMA_SCRIPTS {
        let plain = Arc::new(Graph::new(product_base()));
        let layered = Arc::new(Graph::new(
            LayeredGraph::over(Arc::new(product_base())).unwrap(),
        ));
        let expected = render(Arc::clone(&plain), script);
        let actual = render(Arc::clone(&layered), script);
        failing += usize::from(expected.is_err());
        let expected_state = render(plain, SCHEMA_STATE_SCRIPT);
        let actual_state = render(layered, SCHEMA_STATE_SCRIPT);
        assert!(expected_state.is_ok(), "{script}: {expected_state:?}");
        if expected != actual || expected_state != actual_state {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}\n  state TraversalGraph: {expected_state:?}\n  state LayeredGraph:   {actual_state:?}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    // The refusals compared above are real refusals, not a script typo on both sides.
    assert!(failing >= 4, "{failing}");
}

// ------------------------------------------------------------------------------------------
// Units, savepoints, rollback (S1.8)
// ------------------------------------------------------------------------------------------

/// A layer over modern (with a saved query in its base) that already holds every kind of
/// delta state, written outside any unit: added vertices `a`, `b` with edges among them and to
/// base vertices (a self-loop included), an image of marko (age 30), lop with a gained label,
/// an image of edge `0` (weight 0.9), edge `2` relabeled to `friend`, base edge `3` dropped,
/// a saved query of the layer.
fn prepared_layer() -> LayeredGraph {
    let mut base = TraversalGraph::tinkerpop_modern();
    base.traversal_mut()
        .define_query(
            "people",
            graphersal::catalog::QueryDefinition::new(
                r#"fn people() { g.v().has_label("person") }"#,
            ),
        )
        .unwrap();
    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    let a = layer
        .insert_vertex(
            Some("a".into()),
            vec!["person".into()],
            props(&[("name", "ada".into())]),
        )
        .unwrap();
    let b = layer
        .insert_vertex(
            Some("b".into()),
            vec!["person".into(), "robot".into()],
            props(&[("name", "bob".into())]),
        )
        .unwrap();
    let marko = v(&layer, "1");
    let peter = v(&layer, "6");
    let s = &mut layer;
    s.insert_edge(
        Some("ab".into()),
        Some("knows".into()),
        a,
        b,
        props(&[("w", 1.into())]),
    )
    .unwrap();
    s.insert_edge(
        Some("am".into()),
        Some("likes".into()),
        a,
        marko,
        PropertyMap::default(),
    )
    .unwrap();
    s.insert_edge(
        Some("pa".into()),
        Some("knows".into()),
        peter,
        a,
        PropertyMap::default(),
    )
    .unwrap();
    s.insert_edge(Some("aa".into()), None, a, a, PropertyMap::default())
        .unwrap();
    s.set_vertex_property(marko, "age".into(), 30.into())
        .unwrap();
    let lop = v(s, "3");
    s.add_vertex_label(lop, "product".into()).unwrap();
    let e0 = edge_of(s, "0");
    s.set_edge_property(e0, "weight".into(), 0.9.into())
        .unwrap();
    let e2 = edge_of(s, "2");
    s.set_edge_label(e2, "friend".into()).unwrap();
    let e3 = edge_of(s, "3");
    s.drop_edge(e3).unwrap();
    s.set_definition(graphersal::catalog::Definition::query(
        "mine",
        graphersal::catalog::QueryDefinition::new("fn mine() { 1 }"),
    ))
    .unwrap();
    layer
}

/// Everything about a layer a rollback must restore: the data state, the schema and the
/// catalog.
fn full_state(
    layer: &LayeredGraph,
) -> (
    State,
    Option<GraphSchema>,
    Vec<graphersal::catalog::Definition>,
) {
    (
        state(layer),
        layer.get_schema().unwrap(),
        definitions_of(layer),
    )
}

/// One write of each kind the layer records, each applied to a [`prepared_layer`]: creates,
/// drops of added and base elements (clean, imaged, relabeled, with added edges), property,
/// path and label writes on added and base elements (a fresh copy-up, a write to an image, a
/// return to clean), edge relabels, schema and catalog writes, and several writes to one
/// element.
#[allow(clippy::type_complexity)]
fn layer_write_kinds() -> Vec<(&'static str, Box<dyn Fn(&mut LayeredGraph)>)> {
    fn e(s: &LayeredGraph, id: &str) -> LayeredEdge {
        edge_of(s, id)
    }
    vec![
        (
            "insert a vertex with an explicit id",
            Box::new(|s| {
                s.insert_vertex(
                    Some("n1".into()),
                    vec!["x".into()],
                    props(&[("k", 1.into())]),
                )
                .unwrap();
            }),
        ),
        (
            "add a vertex with an auto id",
            Box::new(|s| {
                s.add_vertex(None, vec!["person".into()], PropertyMap::default())
                    .unwrap();
            }),
        ),
        (
            "insert an edge between added vertices",
            Box::new(|s| {
                let (a, b) = (v(s, "a"), v(s, "b"));
                s.insert_edge(
                    Some("ba".into()),
                    Some("knows".into()),
                    b,
                    a,
                    PropertyMap::default(),
                )
                .unwrap();
            }),
        ),
        (
            "insert an edge between base vertices without stubs",
            Box::new(|s| {
                let (ripple, vadas) = (v(s, "5"), v(s, "2"));
                s.insert_edge(
                    Some("rv".into()),
                    Some("x".into()),
                    ripple,
                    vadas,
                    PropertyMap::default(),
                )
                .unwrap();
            }),
        ),
        (
            "insert an edge from a base vertex to an added one",
            Box::new(|s| {
                let (josh, a) = (v(s, "4"), v(s, "a"));
                s.insert_edge(
                    Some("ja".into()),
                    Some("knows".into()),
                    josh,
                    a,
                    PropertyMap::default(),
                )
                .unwrap();
            }),
        ),
        (
            "drop an added edge",
            Box::new(|s| s.drop_edge(e(s, "ab")).unwrap()),
        ),
        (
            "drop an added vertex with edges to added and base vertices and a self-loop",
            Box::new(|s| s.drop_vertex(v(s, "a")).unwrap()),
        ),
        (
            "drop a clean base edge",
            Box::new(|s| s.drop_edge(e(s, "4")).unwrap()),
        ),
        (
            "drop an imaged base edge",
            Box::new(|s| s.drop_edge(e(s, "0")).unwrap()),
        ),
        (
            "drop a relabeled base edge",
            Box::new(|s| s.drop_edge(e(s, "2")).unwrap()),
        ),
        (
            "drop an imaged base vertex with added edges",
            Box::new(|s| s.drop_vertex(v(s, "1")).unwrap()),
        ),
        (
            "drop a clean base vertex",
            Box::new(|s| s.drop_vertex(v(s, "5")).unwrap()),
        ),
        (
            "drop a base vertex with a gained label",
            Box::new(|s| s.drop_vertex(v(s, "3")).unwrap()),
        ),
        (
            "drop a base vertex with an added edge at its stub",
            Box::new(|s| s.drop_vertex(v(s, "6")).unwrap()),
        ),
        (
            "set a property of a clean base vertex",
            Box::new(|s| {
                s.set_vertex_property(v(s, "2"), "age".into(), 28.into())
                    .unwrap()
            }),
        ),
        (
            "set a property of an imaged base vertex",
            Box::new(|s| {
                s.set_vertex_property(v(s, "1"), "name".into(), "M".into())
                    .unwrap()
            }),
        ),
        (
            "a write that returns an image to clean",
            Box::new(|s| {
                s.set_vertex_property(v(s, "1"), "age".into(), 29.into())
                    .unwrap()
            }),
        ),
        (
            "drop a property of a clean base vertex",
            Box::new(|s| s.drop_vertex_property("age", v(s, "4")).unwrap()),
        ),
        (
            "a path write on a base vertex",
            Box::new(|s| {
                s.set_vertex_property_path(v(s, "2"), &path("address.city"), "Berlin".into())
                    .unwrap()
            }),
        ),
        (
            "a path removal on an imaged base vertex",
            Box::new(|s| {
                s.remove_vertex_property_path(v(s, "1"), &path("name"))
                    .unwrap()
            }),
        ),
        (
            "set a property of a clean base edge",
            Box::new(|s| {
                s.set_edge_property(e(s, "4"), "weight".into(), 0.5.into())
                    .unwrap()
            }),
        ),
        (
            "set a new property of an imaged base edge",
            Box::new(|s| {
                s.set_edge_property(e(s, "0"), "since".into(), 2000.into())
                    .unwrap()
            }),
        ),
        (
            "an edge write that returns its image to clean",
            Box::new(|s| {
                s.set_edge_property(e(s, "0"), "weight".into(), 0.5.into())
                    .unwrap()
            }),
        ),
        (
            "drop a property of a relabeled base edge",
            Box::new(|s| s.drop_edge_property("weight", e(s, "2")).unwrap()),
        ),
        (
            "a path write on a base edge",
            Box::new(|s| {
                s.set_edge_property_path(e(s, "5"), &path("info.by"), "x".into())
                    .unwrap()
            }),
        ),
        (
            "add a label to a clean base vertex",
            Box::new(|s| {
                s.add_vertex_label(v(s, "4"), "employee".into()).unwrap();
            }),
        ),
        (
            "remove a gained label (a return to clean)",
            Box::new(|s| {
                s.remove_vertex_label(v(s, "3"), "product").unwrap();
            }),
        ),
        (
            "remove a base label of an imaged vertex",
            Box::new(|s| {
                s.remove_vertex_label(v(s, "1"), "person").unwrap();
            }),
        ),
        (
            "relabel a clean base edge",
            Box::new(|s| {
                s.set_edge_label(e(s, "1"), "made".into()).unwrap();
            }),
        ),
        (
            "relabel a relabeled base edge back to its base label",
            Box::new(|s| {
                s.set_edge_label(e(s, "2"), "knows".into()).unwrap();
            }),
        ),
        (
            "relabel a relabeled base edge to another label",
            Box::new(|s| {
                s.set_edge_label(e(s, "2"), "mate".into()).unwrap();
            }),
        ),
        (
            "property writes of added elements",
            Box::new(|s| {
                let a = v(s, "a");
                s.set_vertex_property(a, "name".into(), "ann".into())
                    .unwrap();
                s.set_vertex_property(a, "age".into(), 3.into()).unwrap();
                s.drop_vertex_property("name", v(s, "b")).unwrap();
                let ab = e(s, "ab");
                s.set_edge_property(ab, "w".into(), 2.into()).unwrap();
                s.drop_edge_property("w", ab).unwrap();
                s.set_edge_property(e(s, "am"), "since".into(), 1.into())
                    .unwrap();
            }),
        ),
        (
            "label writes of added elements",
            Box::new(|s| {
                let a = v(s, "a");
                s.add_vertex_label(a, "x".into()).unwrap();
                s.remove_vertex_label(a, "person").unwrap();
                s.set_edge_label(e(s, "ab"), "met".into()).unwrap();
                s.set_edge_label(e(s, "aa"), "self".into()).unwrap();
            }),
        ),
        (
            "set the schema",
            Box::new(|s| {
                s.set_schema(GraphSchema::new().with_mode(SchemaMode::Open))
                    .unwrap()
            }),
        ),
        (
            "catalog writes",
            Box::new(|s| {
                use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
                s.set_definition(Definition::query(
                    "q1",
                    QueryDefinition::new("fn q1() { 1 }"),
                ))
                .unwrap();
                s.set_definition(Definition::query(
                    "mine",
                    QueryDefinition::new("fn mine() { 2 }"),
                ))
                .unwrap();
                assert!(
                    s.remove_definition(DefinitionKind::QUERY, "people")
                        .unwrap()
                );
                // A failing list (inside a unit it keeps the ones before it until the unit ends).
                assert!(
                    s.set_definitions(vec![
                        Definition::query("q2", QueryDefinition::new("fn q2() { 1 }")),
                        Definition::query("not valid", QueryDefinition::new("fn x() {}")),
                    ])
                    .is_err()
                );
            }),
        ),
        (
            "several writes to one base vertex, then its drop",
            Box::new(|s| {
                let marko = v(s, "1");
                s.set_vertex_property(marko, "age".into(), 31.into())
                    .unwrap();
                s.add_vertex_label(marko, "x".into()).unwrap();
                s.drop_vertex_property("name", marko).unwrap();
                let n = s
                    .insert_vertex(Some("n2".into()), vec!["n".into()], PropertyMap::default())
                    .unwrap();
                s.insert_edge(Some("mn".into()), None, marko, n, PropertyMap::default())
                    .unwrap();
                s.drop_vertex(marko).unwrap();
            }),
        ),
        (
            "a dropped base id reused, written, dropped again",
            Box::new(|s| {
                s.drop_vertex(v(s, "2")).unwrap();
                let n = s
                    .insert_vertex(
                        Some("2".into()),
                        vec!["person".into()],
                        PropertyMap::default(),
                    )
                    .unwrap();
                s.set_vertex_property(n, "k".into(), 1.into()).unwrap();
                let a = v(s, "a");
                s.insert_edge(Some("2a".into()), None, n, a, PropertyMap::default())
                    .unwrap();
                s.drop_vertex(n).unwrap();
                s.drop_vertex(a).unwrap();
            }),
        ),
        (
            "an added vertex dropped and its id reused",
            Box::new(|s| {
                s.drop_vertex(v(s, "b")).unwrap();
                s.insert_vertex(Some("b".into()), vec!["y".into()], PropertyMap::default())
                    .unwrap();
            }),
        ),
    ]
}

/// The handles of every element, with their ids.
fn handles(layer: &LayeredGraph) -> (Vec<(LayeredVertex, String)>, Vec<(LayeredEdge, String)>) {
    let vs = layer
        .vertices()
        .unwrap()
        .map(|n| (n, layer.vertex_id(n).unwrap().unwrap().to_string()))
        .collect();
    let es = layer
        .edges()
        .unwrap()
        .map(|e| (e, layer.edge_id(e).unwrap().unwrap().to_string()))
        .collect();
    (vs, es)
}

/// After a rollback every handle taken before the unit names its element again (a base
/// element always: its handle never changes), except the handle of an added element the unit
/// dropped: that one is stale and the element is back under a new handle, as in a
/// `TraversalGraph`.
fn assert_handles_after_rollback(
    layer: &LayeredGraph,
    before: &(Vec<(LayeredVertex, String)>, Vec<(LayeredEdge, String)>),
    kind: &str,
) {
    for (node, id) in &before.0 {
        if layer.contains_vertex(*node).unwrap() {
            assert_eq!(layer.vertex_id(*node).unwrap(), Some(id.as_str()), "{kind}");
        } else {
            let in_base = GraphStorage::get_vertex_by_id(&**layer.base(), id).unwrap();
            assert!(
                in_base.is_none(),
                "{kind}: base vertex {id} must stay addressable"
            );
            assert!(
                layer.get_vertex_by_id(id).unwrap().is_some(),
                "{kind}: {id}"
            );
        }
    }
    for (edge, id) in &before.1 {
        if layer.contains_edge(*edge).unwrap() {
            assert_eq!(layer.edge_id(*edge).unwrap(), Some(id.as_str()), "{kind}");
        } else {
            let in_base = GraphStorage::get_edge_by_id(&**layer.base(), id).unwrap();
            assert!(
                in_base.is_none(),
                "{kind}: base edge {id} must stay addressable"
            );
            assert!(layer.get_edge_by_id(id).unwrap().is_some(), "{kind}: {id}");
        }
    }
}

impl State {
    /// The state with label buckets and adjacency lists sorted (scans keep their order): the
    /// comparison after an undo that stored an added element again, which puts it at the front
    /// of its endpoints' adjacency lists and at the end of its label buckets, exactly as a
    /// `TraversalGraph`'s undo of a drop does (`graph/undo.rs`: "Not restored: iteration
    /// order"), or moved a vertex out of a label bucket and back.
    fn with_sorted_lists(self) -> Self {
        let mut state = self.with_sorted_buckets();
        for vertex in &mut state.vertices {
            vertex.3.sort();
            vertex.4.sort();
        }
        state
    }
}

/// [`full_state`] with [`State::with_sorted_lists`].
fn sorted_full_state(
    layer: &LayeredGraph,
) -> (
    State,
    Option<GraphSchema>,
    Vec<graphersal::catalog::Definition>,
) {
    let (state, schema, catalog) = full_state(layer);
    (state.with_sorted_lists(), schema, catalog)
}

/// For each write kind: a unit that writes and then rolls back leaves exactly the prior state
/// (data, statistics equal to a recount, schema, catalog; scans, adjacency and label buckets in
/// order), the handles taken before still work, the unit reported its change, and the counts
/// of the commit sequence did not move. Kinds whose undo stores an added element again (or
/// moves a vertex out of a label bucket and back) may differ only in the order of adjacency
/// lists and label buckets, as a `TraversalGraph`'s undo does ([`State::with_sorted_lists`]).
#[test]
fn a_rolled_back_unit_restores_every_write_kind() {
    let mut reordered = Vec::new();
    for (kind, write) in layer_write_kinds() {
        let mut layer = prepared_layer();
        let before = full_state(&layer);
        let held = handles(&layer);
        let seq = layer.last_commit_seq().unwrap();
        let mark = layer.begin_unit().unwrap();
        write(&mut layer);
        let changes = layer.unit_changes(mark).unwrap();
        assert!(changes.any(), "{kind}: the unit reports its change");
        layer
            .rollback_unit(mark, &RollbackReason::failed(&"test"))
            .unwrap();
        let after = full_state(&layer);
        if after != before {
            assert_eq!(
                sorted_full_state(&layer),
                sorted_full_state(&prepared_layer()),
                "{kind}: a rollback restores everything"
            );
            reordered.push(kind);
        }
        assert_handles_after_rollback(&layer, &held, kind);
        assert_eq!(layer.last_commit_seq().unwrap(), seq, "{kind}");
        assert_eq!(
            layer.unit_started_at().unwrap(),
            None,
            "{kind}: the unit is closed"
        );
        // The layer keeps working: the same write again, committed this time.
        let mark = layer.begin_unit().unwrap();
        write(&mut layer);
        layer.commit_unit(mark).unwrap();
        assert_eq!(layer.last_commit_seq().unwrap(), seq + 1, "{kind}");
        if kind.contains("auto id") {
            continue;
        }
        let mut again = prepared_layer();
        let mark = again.begin_unit().unwrap();
        write(&mut again);
        again.commit_unit(mark).unwrap();
        assert_eq!(
            sorted_full_state(&layer),
            sorted_full_state(&again),
            "{kind}: the write after a rollback gives what it gives on a fresh layer"
        );
    }
    // Only kinds that store an added element again or move a vertex out of a label bucket
    // and back may come back with a list in another order.
    println!("order-only differences: {reordered:?}");
    for kind in &reordered {
        assert!(
            kind.contains("drop") || kind.contains("label"),
            "{kind}: an unexpected order difference"
        );
    }
}

/// Nested units are savepoints: rolling back the inner unit undoes only its writes (also when
/// they hit the same base element as the outer unit's: a second image write, a label, a drop),
/// the outer unit's stay and commit as one commit; `unit_changes` follows.
#[test]
fn a_savepoint_rollback_keeps_the_outer_unit() {
    let mut layer = prepared_layer();
    let seq = layer.last_commit_seq().unwrap();
    let outer = layer.begin_unit().unwrap();
    let marko = v(&layer, "1");
    let vadas = v(&layer, "2");
    layer
        .set_vertex_property(marko, "age".into(), 40.into())
        .unwrap();
    layer.add_vertex_label(vadas, "x".into()).unwrap();
    layer
        .insert_vertex(Some("kept".into()), vec![], PropertyMap::default())
        .unwrap();
    let after_outer = full_state(&layer);
    let inner = layer.begin_unit().unwrap();
    assert!(
        !layer.unit_changes(inner).unwrap().any(),
        "a fresh savepoint"
    );
    layer
        .set_vertex_property(marko, "age".into(), 41.into())
        .unwrap();
    layer.remove_vertex_label(vadas, "person").unwrap();
    layer.drop_vertex(vadas).unwrap();
    layer.drop_vertex(v(&layer, "a")).unwrap();
    layer
        .set_schema(GraphSchema::new().with_mode(SchemaMode::Open))
        .unwrap();
    let changes = layer.unit_changes(inner).unwrap();
    assert!(changes.data && changes.schema && !changes.definitions);
    layer
        .rollback_unit(inner, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(
        sorted_full_state(&layer),
        {
            let (state, schema, catalog) = after_outer;
            (state.with_sorted_lists(), schema, catalog)
        },
        "the savepoint's writes are gone, the outer unit's stay"
    );
    let changes = layer.unit_changes(outer).unwrap();
    assert!(changes.data && !changes.schema, "{changes:?}");
    layer.commit_unit(outer).unwrap();
    assert_eq!(layer.last_commit_seq().unwrap(), seq + 1, "one commit");
    assert_eq!(
        layer.vertex_properties(marko).unwrap().unwrap().get("age"),
        Some(&40.into())
    );
    assert!(layer.vertex_has_label(vadas, "x").unwrap());
    assert!(layer.get_vertex_by_id("kept").unwrap().is_some());
    assert!(layer.get_schema().unwrap().is_none());
}

/// A committed unit keeps everything; a unit that changed nothing is no commit; writes
/// outside a unit are not recorded (and keep working); a rolled-back unit leaves the auto-id
/// sequences advanced, as a `TraversalGraph` does.
#[test]
fn commits_keep_and_sequences_are_never_rolled_back() {
    let mut layer = modern_layer();
    let mut reference = TraversalGraph::tinkerpop_modern();
    fn run<S: GraphStorage>(s: &mut S) -> (u64, Vec<String>) {
        let start = s.last_commit_seq().unwrap();
        let mark = s.begin_unit().unwrap();
        let _ = s.vertex_count().unwrap();
        s.commit_unit(mark).unwrap();
        assert_eq!(
            s.last_commit_seq().unwrap(),
            start,
            "a read-only unit is no commit"
        );
        let mark = s.begin_unit().unwrap();
        let undone = s
            .insert_vertex(None, vec!["n".into()], PropertyMap::default())
            .unwrap();
        let undone_id = s.vertex_id(undone).unwrap().unwrap().to_string();
        s.rollback_unit(mark, &RollbackReason::failed(&"test"))
            .unwrap();
        assert!(
            !s.contains_vertex(undone).unwrap(),
            "a handle taken inside is stale"
        );
        let mark = s.begin_unit().unwrap();
        let kept = s
            .insert_vertex(None, vec!["n".into()], PropertyMap::default())
            .unwrap();
        let kept_id = s.vertex_id(kept).unwrap().unwrap().to_string();
        let marko = s.get_vertex_by_id("1").unwrap().unwrap();
        s.set_vertex_property(marko, "age".into(), 30.into())
            .unwrap();
        s.commit_unit(mark).unwrap();
        // Outside a unit: kept, no commit counted.
        let josh = s.get_vertex_by_id("4").unwrap().unwrap();
        s.drop_vertex(josh).unwrap();
        (
            s.last_commit_seq().unwrap() - start,
            vec![undone_id, kept_id],
        )
    }
    let on_layer = run(&mut layer);
    let on_reference = run(&mut reference);
    assert_eq!(on_layer, on_reference);
    assert_eq!(on_layer.0, 1);
    assert_ne!(
        on_layer.1[0], on_layer.1[1],
        "an auto id is never handed out twice"
    );
    assert_eq!(state(&layer), state(&reference));
}

/// `set_definitions`: inside a unit a refused definition keeps the ones before it until the
/// unit ends (its rollback removes them); outside a unit the list is a unit of its own and a
/// refusal leaves nothing; `set_schema` and catalog writes outside a unit are commits of their
/// own. All as on a `TraversalGraph`.
#[test]
fn catalog_and_schema_writes_are_units_of_their_own() {
    use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
    fn run<S: GraphStorage>(s: &mut S) -> Vec<String> {
        let mut seen = Vec::new();
        let list = || {
            vec![
                Definition::query("q1", QueryDefinition::new("fn q1() { 1 }")),
                Definition::query("not valid", QueryDefinition::new("fn x() {}")),
            ]
        };
        let names = |s: &S| -> String {
            definitions_of(s)
                .iter()
                .map(|d| d.name().to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        let start = s.last_commit_seq().unwrap();
        assert!(s.set_definitions(list()).is_err());
        seen.push(format!(
            "outside: {} seq+{}",
            names(s),
            s.last_commit_seq().unwrap() - start
        ));
        let mark = s.begin_unit().unwrap();
        assert!(s.set_definitions(list()).is_err());
        seen.push(format!("inside: {}", names(s)));
        s.rollback_unit(mark, &RollbackReason::failed(&"test"))
            .unwrap();
        seen.push(format!("rolled back: {}", names(s)));
        s.set_definition(Definition::query(
            "q2",
            QueryDefinition::new("fn q2() { 2 }"),
        ))
        .unwrap();
        s.set_schema(GraphSchema::new().with_mode(SchemaMode::Open))
            .unwrap();
        s.remove_definitions(&[(DefinitionKind::QUERY, "q2"), (DefinitionKind::QUERY, "zz")])
            .unwrap();
        seen.push(format!(
            "committed: {} seq+{}",
            names(s),
            s.last_commit_seq().unwrap() - start
        ));
        seen
    }
    let on_layer = run(&mut modern_layer());
    let on_reference = run(&mut TraversalGraph::tinkerpop_modern());
    assert_eq!(on_layer, on_reference);
    assert_eq!(on_layer[1], "inside: q1");
}

/// `Transactional::transaction` on a bare layer and on `Graph<LayeredGraph>`: `Ok` commits,
/// `Err` rolls everything back, a failing nested transaction is a savepoint. `dry_run`
/// reports its changes and leaves the layer as it was (S1.9).
/// Compared with sorted lists: the rolled-back drop of marko stores the added edge `am` again
/// ([`State::with_sorted_lists`]).
#[test]
fn transactions_and_dry_runs_on_a_layer() {
    let mut layer = prepared_layer();
    let before = sorted_full_state(&layer);
    let seq = layer.last_commit_seq().unwrap();
    let failed: Result<(), GraphError> = layer.transaction(|l| {
        let marko = l.get_vertex_by_id("1").unwrap().unwrap();
        l.drop_vertex(marko)?;
        l.insert_vertex(Some("t".into()), vec![], PropertyMap::default())?;
        Err(GraphError::unsupported("the closure gives up"))
    });
    assert!(failed.is_err());
    assert_eq!(
        sorted_full_state(&layer),
        before,
        "an Err rolls everything back"
    );
    let kept: Result<(), GraphError> = layer.transaction(|l| {
        l.insert_vertex(Some("t".into()), vec![], PropertyMap::default())?;
        let nested: Result<(), GraphError> = l.transaction(|l| {
            let lop = l.get_vertex_by_id("3").unwrap().unwrap();
            l.drop_vertex(lop)?;
            Err(GraphError::unsupported("nested gives up"))
        });
        assert!(nested.is_err());
        assert!(
            l.get_vertex_by_id("3").unwrap().is_some(),
            "the savepoint undid the drop"
        );
        Ok(())
    });
    kept.unwrap();
    assert!(layer.get_vertex_by_id("t").unwrap().is_some());
    assert_eq!(
        layer.last_commit_seq().unwrap(),
        seq + 1,
        "the failed transaction is no commit, the kept one is one"
    );

    let before = sorted_full_state(&layer);
    let seq = layer.last_commit_seq().unwrap();
    let ((), dry) = layer
        .dry_run(|l| {
            let josh = l.get_vertex_by_id("4").unwrap().unwrap();
            l.drop_vertex(josh)
        })
        .unwrap();
    let dropped: Vec<&str> = dry
        .mutations()
        .iter()
        .map(|m| match m {
            graphersal::changes::Mutation::DropEdge { .. } => "edge",
            graphersal::changes::Mutation::DropVertex { .. } => "vertex",
            _ => "other",
        })
        .collect();
    assert_eq!(
        dropped.last(),
        Some(&"vertex"),
        "a dry run reports its changes, the cascaded edges first: {dry:?}"
    );
    assert!(dropped.len() > 1 && dropped[..dropped.len() - 1].iter().all(|m| *m == "edge"));
    assert_eq!(
        sorted_full_state(&layer),
        before,
        "the dry run's writes were rolled back"
    );
    assert_eq!(
        layer.last_commit_seq().unwrap(),
        seq,
        "a dry run is no commit"
    );

    let graph = Graph::new(prepared_layer());
    let before = sorted_full_state(&graph.read());
    let failed: Result<(), GraphError> = graph.write().transaction(|l| {
        let e0 = l.get_edge_by_id("0").unwrap().unwrap();
        l.set_edge_label(e0, "x".into())?;
        Err(GraphError::unsupported("give up"))
    });
    assert!(failed.is_err());
    assert_eq!(sorted_full_state(&graph.read()), before);
    assert!(!graph.is_poisoned());
}

/// Scripts whose traversal writes and then fails (each traversal is a unit of its own): the
/// failing traversal leaves nothing, on the layer as on the plain graph, and what earlier
/// traversals of the script committed stays. Gap of task S1.3, closed by the layer's units.
#[cfg(feature = "script")]
const RHAI_FAILING_WRITES: &[&str] = &[
    r#"g.V().property("seen", true).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.addV("ghost").property("name", "g").constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("1").addE("likes").to(__.V("3")).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("1").add_label("employee").constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.E().hasLabel("knows").property("weight", 9).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("2").sideEffect(__.drop()).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("1").sideEffect(__.outE("knows").drop()).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("4").sideEffect(__.properties("age").drop()).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("1").property(jpath("address.city"), "Berlin").constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.addV("kept").iterate(); g.V("4").property("age", 1).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("1").addE("x").to(__.V("2")).iterate(); g.V("1").sideEffect(__.drop()).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"let a = g.addV("n").id().next(); g.V(a).addE("e").to(__.V("1")).iterate(); g.V(a).sideEffect(__.drop()).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("1").property("age", 30).iterate(); g.V("1").property("age", 29).constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.V("3").add_label("x").iterate(); g.V("3").drop_label("x").constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.E("0").set_label("met").constant("x").asNumber(GType.LONG).iterate()"#,
    r#"g.E("2").set_label("met").iterate(); g.E("2").set_label("mate").constant("x").asNumber(GType.LONG).iterate()"#,
];

/// The state after a script, with every list sorted: the comparison when a rollback stored
/// an element again (a `TraversalGraph` puts a restored element at the front of its
/// neighbours' lists, `graph/undo.rs`; the layer puts a base element back where the base
/// holds it).
#[cfg(feature = "script")]
const UNORDERED_STATE_SCRIPT: &str = r#"[
    g.V().order().by(T.id).elementMap().toList(),
    g.E().order().by(T.id).elementMap().toList(),
    g.V().order().by(T.id).project("v", "out", "in").by(T.id).by(__.outE().id().order().fold()).by(__.inE().id().order().fold()).toList(),
    g.statistics()
]"#;

/// What a script did on one graph: its outcome, the commit position afterwards, the exact and
/// the unordered state.
#[cfg(feature = "script")]
type Outcome = (
    Result<String, String>,
    u64,
    Result<String, String>,
    Result<String, String>,
);

/// Runs `script` on `graph` (as one unit with `atomic`, else a unit per traversal).
#[cfg(feature = "script")]
fn outcome<S>(graph: Arc<Graph<S>>, script: &str, atomic: bool) -> Outcome
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    let result = if atomic {
        crate::script_host::eval_value_atomic(
            Arc::clone(&graph),
            &format!("({{ {script} }}).to_string()"),
            HashMap::new(),
            &crate::script_host::ScriptLimits::default(),
        )
        .map(|value| value.to_string())
        .map_err(|err| err.to_string())
    } else {
        render(Arc::clone(&graph), script)
    };
    let seq = graph.read().last_commit_seq().unwrap();
    (
        result,
        seq,
        render(Arc::clone(&graph), CROSS_STATE_SCRIPT),
        render(graph, UNORDERED_STATE_SCRIPT),
    )
}

/// Each script on the plain modern graph and on a layer over it: the same outcome, the same
/// commit count and the same state, exactly; or, where a rollback stored an element again,
/// the same with sorted lists (returned as order-only). Also returns how many scripts failed.
#[cfg(feature = "script")]
fn rollback_differential(
    scripts: &[&'static str],
    atomic: bool,
) -> (Vec<String>, Vec<&'static str>, usize) {
    let mut differences = Vec::new();
    let mut order_only = Vec::new();
    let mut failed = 0;
    for script in scripts {
        let expected = outcome(plain_graph(), script, atomic);
        let actual = outcome(layered_graph(), script, atomic);
        failed += usize::from(expected.0.is_err());
        assert!(expected.2.is_ok(), "{script}: {:?}", expected.2);
        if expected == actual {
            continue;
        }
        if expected.0 == actual.0 && expected.1 == actual.1 && expected.3 == actual.3 {
            order_only.push(*script);
            continue;
        }
        differences.push(format!(
            "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}"
        ));
    }
    (differences, order_only, failed)
}

#[cfg(feature = "script")]
#[test]
fn rhai_failing_writes_leave_nothing_like_the_plain_graph() {
    let (differences, order_only, failed) = rollback_differential(RHAI_FAILING_WRITES, false);
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    assert_eq!(failed, RHAI_FAILING_WRITES.len(), "every script fails");
    println!(
        "order-only differences: {}\n{}",
        order_only.len(),
        order_only.join("\n")
    );
    // Only a rolled-back drop stores elements again (in another list order on the plain
    // graph).
    assert!(
        order_only.iter().all(|script| script.contains("drop()")),
        "{order_only:?}"
    );
}

/// Whole-script units (`eval_value_atomic`): a script that succeeds commits once, one that
/// fails (a `throw`, a failing traversal, a schema violation) leaves nothing, including its
/// schema and catalog changes; a failing traversal caught inside the script is a savepoint.
#[cfg(feature = "script")]
const RHAI_ATOMIC: &[&str] = &[
    r#"g.addV("person").property("name", "bo").next(); g.V("6").drop().iterate(); g.V().count().next()"#,
    r#"g.addV("person").property("name", "bo").next(); throw "boom""#,
    r#"g.V("1").property("age", 30).iterate(); g.V("1").values("age").next()"#,
    r#"g.V("1").property("age", 30).iterate(); g.V("1").add_label("x").iterate(); g.E("0").property("weight", 2.0).iterate(); g.V("2").properties("age").drop().iterate(); throw "boom""#,
    r#"g.V("1").property("age", 30).iterate(); g.V("1").property("age", 29).iterate(); throw "boom""#,
    r#"g.V("1").drop().iterate(); throw "boom""#,
    r#"g.E().hasLabel("knows").drop().iterate(); g.V("3").drop().iterate(); throw "boom""#,
    r#"g.V("1").addE("self").to(__.V("1")).iterate(); g.V("2").drop().iterate(); g.addV("n").property(T.id, "2").iterate(); throw "boom""#,
    r#"let a = g.addV("n").id().next(); g.V(a).addE("e").to(__.V("1")).iterate(); g.V("1").drop().iterate(); g.V(a).drop().iterate(); throw "boom""#,
    r#"g.set_schema(g.infer_schema(), SchemaMode.closed); g.addV("robot").next()"#,
    r#"g.set_schema(g.infer_schema(), SchemaMode.open); g.addV("robot").property("x", 1).iterate(); throw "boom""#,
    r#"g.define_query(#{name: "n", body: "fn n() { 1 }"}); throw "boom""#,
    r#"g.E("0").set_label("met").iterate(); g.E("2").set_label("met").iterate(); g.E("0").set_label("knows").iterate(); throw "boom""#,
    r#"g.define_query(#{name: "n", body: "fn n() { 1 }"}); g.query("n").next()"#,
    r#"try { g.V("1").property("age", 1).constant("x").asNumber(GType.LONG).iterate() } catch {} ; g.V("1").property("name", "m").iterate(); g.V("1").valueMap().next()"#,
    r#"g.addV("a").iterate(); try { g.V("2").sideEffect(__.drop()).constant("x").asNumber(GType.LONG).iterate() } catch {} ; [g.V().count().next(), g.E().count().next()]"#,
];

#[cfg(feature = "script")]
#[test]
fn rhai_atomic_scripts_match_the_plain_graph() {
    let (differences, order_only, failed) = rollback_differential(RHAI_ATOMIC, true);
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    assert!(failed >= 8, "the failing scripts really fail: {failed}");
    println!(
        "order-only differences: {}\n{}",
        order_only.len(),
        order_only.join("\n")
    );
    assert!(
        order_only.iter().all(|script| script.contains("drop()")),
        "{order_only:?}"
    );
}

// ------------------------------------------------------------------------------------------
// Change capture, commit hooks, dry runs (S1.9)
// ------------------------------------------------------------------------------------------

use graphersal::changes::{ChangeSet, CommitHook, CommitMetadata, HookError, Mutation};

/// A hook recording its events (`before:<n mutations>`, `after`, `rollback:<reason>`), keeping
/// every change set it accepted, vetoing while `veto` is set and panicking in `before_commit`
/// while `panic` is set.
#[derive(Clone, Default)]
struct Recorder {
    events: Arc<Mutex<Vec<String>>>,
    accepted: Arc<Mutex<Vec<ChangeSet>>>,
    veto: Arc<Mutex<bool>>,
    panic: Arc<Mutex<bool>>,
}

/// The error a vetoing [`Recorder`] returns.
#[derive(Debug)]
struct NotToday;

impl std::fmt::Display for NotToday {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not today")
    }
}

impl std::error::Error for NotToday {}

impl CommitHook for Recorder {
    fn name(&self) -> &str {
        "recorder"
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.log(format!("before:{}", changes.len()));
        if *self.panic.lock().unwrap() {
            panic!("the recorder panics");
        }
        if *self.veto.lock().unwrap() {
            return Err(Box::new(NotToday));
        }
        self.accepted.lock().unwrap().push(changes.clone());
        Ok(())
    }

    fn after_commit(&mut self, _: &ChangeSet) {
        self.log("after".into());
    }

    fn after_rollback(&mut self, reason: &RollbackReason) {
        let reason = match reason {
            RollbackReason::Vetoed { hook, .. } => format!("vetoed by {hook}"),
            RollbackReason::Failed { .. } => format!("failed: {}", reason.message()),
            _ => "other".into(),
        };
        self.log(format!("rollback:{reason}"));
    }
}

impl Recorder {
    fn log(&self, event: String) {
        self.events.lock().unwrap().push(event);
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    /// The mutations of every accepted change set, oldest first.
    fn mutations(&self) -> Vec<Vec<Mutation>> {
        self.accepted
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.mutations().to_vec())
            .collect()
    }

    /// A recorder registered on `s`.
    fn on<S: GraphStorage>(s: &mut S) -> Self {
        let recorder = Self::default();
        s.register_commit_hook(Box::new(recorder.clone())).unwrap();
        recorder
    }
}

/// The writes of [`prepared_layer`] (after its base), on any storage holding modern.
fn prepare_like_the_layer<S: GraphStorage>(s: &mut S) {
    let a = s
        .insert_vertex(
            Some("a".into()),
            vec!["person".into()],
            props(&[("name", "ada".into())]),
        )
        .unwrap();
    let b = s
        .insert_vertex(
            Some("b".into()),
            vec!["person".into(), "robot".into()],
            props(&[("name", "bob".into())]),
        )
        .unwrap();
    let marko = vertex_of(s, "1");
    let peter = vertex_of(s, "6");
    s.insert_edge(
        Some("ab".into()),
        Some("knows".into()),
        a,
        b,
        props(&[("w", 1.into())]),
    )
    .unwrap();
    s.insert_edge(
        Some("am".into()),
        Some("likes".into()),
        a,
        marko,
        PropertyMap::default(),
    )
    .unwrap();
    s.insert_edge(
        Some("pa".into()),
        Some("knows".into()),
        peter,
        a,
        PropertyMap::default(),
    )
    .unwrap();
    s.insert_edge(Some("aa".into()), None, a, a, PropertyMap::default())
        .unwrap();
    s.set_vertex_property(marko, "age".into(), 30.into())
        .unwrap();
    s.add_vertex_label(vertex_of(s, "3"), "product".into())
        .unwrap();
    s.set_edge_property(edge_of(s, "0"), "weight".into(), 0.9.into())
        .unwrap();
    s.set_edge_label(edge_of(s, "2"), "friend".into()).unwrap();
    s.drop_edge(edge_of(s, "3")).unwrap();
    s.set_definition(graphersal::catalog::Definition::query(
        "mine",
        graphersal::catalog::QueryDefinition::new("fn mine() { 1 }"),
    ))
    .unwrap();
}

/// A `TraversalGraph` holding what [`prepared_layer`] reads: modern, the base's saved query,
/// then the same writes.
fn prepared_reference() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .define_query(
            "people",
            graphersal::catalog::QueryDefinition::new(
                r#"fn people() { g.v().has_label("person") }"#,
            ),
        )
        .unwrap();
    prepare_like_the_layer(&mut graph);
    graph
}

/// Everything about a storage, every list sorted (adjacency, label buckets; elements by id),
/// with schema and catalog: what two storages holding the same data answer whatever order
/// their writes and undos left.
fn canonical<S: GraphStorage>(
    s: &S,
) -> (
    State,
    Option<GraphSchema>,
    Vec<graphersal::catalog::Definition>,
) {
    let mut state = sorted_state(s);
    for vertex in &mut state.vertices {
        vertex.3.sort();
        vertex.4.sort();
    }
    (state, s.get_schema().unwrap(), definitions_of(s))
}

/// The names of the write kinds of [`apply_capture_kind`].
const CAPTURE_KINDS: &[&str] = &[
    "insert a vertex with an explicit id",
    "add a vertex with an auto id",
    "insert an edge between added vertices",
    "insert an edge between base vertices without stubs",
    "insert an edge from a base vertex to an added one",
    "add an edge with an auto id",
    "drop an added edge",
    "drop an added vertex with edges to added and base vertices and a self-loop",
    "drop a clean base edge",
    "drop an imaged base edge",
    "drop a relabeled base edge",
    "drop an imaged base vertex with added edges",
    "drop a clean base vertex",
    "drop a base vertex with a gained label",
    "drop a base vertex with an added edge at its stub",
    "set a property of a clean base vertex",
    "set a property of an imaged base vertex",
    "a write that returns an image to clean",
    "drop a property of a clean base vertex",
    "drop an absent property of a base vertex",
    "a path write on a base vertex",
    "a path removal on an imaged base vertex",
    "nested path writes and a nested path removal on a base vertex",
    "set a property of a clean base edge",
    "set a new property of an imaged base edge",
    "an edge write that returns its image to clean",
    "drop a property of a relabeled base edge",
    "a path write on a base edge",
    "identical values written to base elements",
    "identical values written to added elements",
    "add a label to a clean base vertex",
    "remove a gained label (a return to clean)",
    "remove a base label of an imaged vertex",
    "relabel a clean base edge",
    "relabel a relabeled base edge back to its base label",
    "relabel a relabeled base edge to another label",
    "property writes of added elements",
    "label writes of added elements",
    "set the schema",
    "catalog writes",
    "several writes to one base vertex, then its drop",
    "a dropped base id reused, written, dropped again",
    "an added vertex dropped and its id reused",
    "writes undone by a savepoint",
    "an element added and written in the unit, then dropped",
];

/// One write kind (by name, [`CAPTURE_KINDS`]) on any storage holding [`prepared_layer`]'s
/// data.
fn apply_capture_kind<S: GraphStorage>(s: &mut S, kind: &str) {
    let v = |s: &S, id: &str| vertex_of(s, id);
    let e = |s: &S, id: &str| edge_of(s, id);
    match kind {
        "insert a vertex with an explicit id" => {
            s.insert_vertex(
                Some("n1".into()),
                vec!["x".into()],
                props(&[("k", 1.into())]),
            )
            .unwrap();
        }
        "add a vertex with an auto id" => {
            s.add_vertex(None, vec!["person".into()], PropertyMap::default())
                .unwrap();
        }
        "insert an edge between added vertices" => {
            let (a, b) = (v(s, "a"), v(s, "b"));
            s.insert_edge(
                Some("ba".into()),
                Some("knows".into()),
                b,
                a,
                PropertyMap::default(),
            )
            .unwrap();
        }
        "insert an edge between base vertices without stubs" => {
            let (ripple, vadas) = (v(s, "5"), v(s, "2"));
            s.insert_edge(
                Some("rv".into()),
                Some("x".into()),
                ripple,
                vadas,
                props(&[("w", 2.into())]),
            )
            .unwrap();
        }
        "insert an edge from a base vertex to an added one" => {
            let (josh, a) = (v(s, "4"), v(s, "a"));
            s.insert_edge(
                Some("ja".into()),
                Some("knows".into()),
                josh,
                a,
                PropertyMap::default(),
            )
            .unwrap();
        }
        "add an edge with an auto id" => {
            let (marko, b) = (v(s, "1"), v(s, "b"));
            s.add_edge(None, Some("knows".into()), marko, b, PropertyMap::default())
                .unwrap();
        }
        "drop an added edge" => s.drop_edge(e(s, "ab")).unwrap(),
        "drop an added vertex with edges to added and base vertices and a self-loop" => {
            s.drop_vertex(v(s, "a")).unwrap()
        }
        "drop a clean base edge" => s.drop_edge(e(s, "4")).unwrap(),
        "drop an imaged base edge" => s.drop_edge(e(s, "0")).unwrap(),
        "drop a relabeled base edge" => s.drop_edge(e(s, "2")).unwrap(),
        "drop an imaged base vertex with added edges" => s.drop_vertex(v(s, "1")).unwrap(),
        "drop a clean base vertex" => s.drop_vertex(v(s, "5")).unwrap(),
        "drop a base vertex with a gained label" => s.drop_vertex(v(s, "3")).unwrap(),
        "drop a base vertex with an added edge at its stub" => s.drop_vertex(v(s, "6")).unwrap(),
        "set a property of a clean base vertex" => s
            .set_vertex_property(v(s, "2"), "age".into(), 28.into())
            .unwrap(),
        "set a property of an imaged base vertex" => s
            .set_vertex_property(v(s, "1"), "name".into(), "M".into())
            .unwrap(),
        "a write that returns an image to clean" => s
            .set_vertex_property(v(s, "1"), "age".into(), 29.into())
            .unwrap(),
        "drop a property of a clean base vertex" => {
            s.drop_vertex_property("age", v(s, "4")).unwrap()
        }
        "drop an absent property of a base vertex" => {
            s.drop_vertex_property("nope", v(s, "4")).unwrap();
            s.drop_edge_property("nope", e(s, "4")).unwrap();
            // Something real, so the unit commits.
            s.set_vertex_property(v(s, "2"), "k".into(), 1.into())
                .unwrap();
        }
        "a path write on a base vertex" => s
            .set_vertex_property_path(v(s, "2"), &path("address.city"), "Berlin".into())
            .unwrap(),
        "a path removal on an imaged base vertex" => s
            .remove_vertex_property_path(v(s, "1"), &path("name"))
            .unwrap(),
        "nested path writes and a nested path removal on a base vertex" => {
            let vadas = v(s, "2");
            s.set_vertex_property_path(vadas, &path("address.city"), "Berlin".into())
                .unwrap();
            s.set_vertex_property_path(vadas, &path("address.zip"), "10115".into())
                .unwrap();
            s.remove_vertex_property_path(vadas, &path("address.city"))
                .unwrap();
            s.remove_vertex_property_path(vadas, &path("address.none"))
                .unwrap();
        }
        "set a property of a clean base edge" => s
            .set_edge_property(e(s, "4"), "weight".into(), 0.5.into())
            .unwrap(),
        "set a new property of an imaged base edge" => s
            .set_edge_property(e(s, "0"), "since".into(), 2000.into())
            .unwrap(),
        "an edge write that returns its image to clean" => s
            .set_edge_property(e(s, "0"), "weight".into(), 0.5.into())
            .unwrap(),
        "drop a property of a relabeled base edge" => {
            s.drop_edge_property("weight", e(s, "2")).unwrap()
        }
        "a path write on a base edge" => s
            .set_edge_property_path(e(s, "5"), &path("info.by"), "x".into())
            .unwrap(),
        "identical values written to base elements" => {
            let vadas = v(s, "2");
            let age = s
                .vertex_properties(vadas)
                .unwrap()
                .unwrap()
                .get("age")
                .cloned()
                .unwrap();
            s.set_vertex_property(vadas, "age".into(), age).unwrap();
            // An imaged vertex, its image's value.
            s.set_vertex_property(v(s, "1"), "age".into(), 30.into())
                .unwrap();
            s.set_vertex_property_path(v(s, "4"), &path("name"), "josh".into())
                .unwrap();
            let e4 = e(s, "4");
            let weight = s
                .edge_properties(e4)
                .unwrap()
                .unwrap()
                .get("weight")
                .cloned()
                .unwrap();
            s.set_edge_property(e4, "weight".into(), weight).unwrap();
            s.set_edge_property(e(s, "0"), "weight".into(), 0.9.into())
                .unwrap();
            // Equal labels are no writes at all (on both).
            s.add_vertex_label(v(s, "3"), "product".into()).unwrap();
            s.set_edge_label(e(s, "2"), "friend".into()).unwrap();
        }
        "identical values written to added elements" => {
            s.set_vertex_property(v(s, "a"), "name".into(), "ada".into())
                .unwrap();
            s.set_edge_property(e(s, "ab"), "w".into(), 1.into())
                .unwrap();
        }
        "add a label to a clean base vertex" => {
            s.add_vertex_label(v(s, "4"), "employee".into()).unwrap();
        }
        "remove a gained label (a return to clean)" => {
            s.remove_vertex_label(v(s, "3"), "product").unwrap();
        }
        "remove a base label of an imaged vertex" => {
            s.remove_vertex_label(v(s, "1"), "person").unwrap();
        }
        "relabel a clean base edge" => {
            s.set_edge_label(e(s, "1"), "made".into()).unwrap();
        }
        "relabel a relabeled base edge back to its base label" => {
            s.set_edge_label(e(s, "2"), "knows".into()).unwrap();
        }
        "relabel a relabeled base edge to another label" => {
            s.set_edge_label(e(s, "2"), "mate".into()).unwrap();
        }
        "property writes of added elements" => {
            let a = v(s, "a");
            s.set_vertex_property(a, "name".into(), "ann".into())
                .unwrap();
            s.set_vertex_property(a, "age".into(), 3.into()).unwrap();
            s.drop_vertex_property("name", v(s, "b")).unwrap();
            let ab = e(s, "ab");
            s.set_edge_property(ab, "w".into(), 2.into()).unwrap();
            s.drop_edge_property("w", ab).unwrap();
            s.set_edge_property(e(s, "am"), "since".into(), 1.into())
                .unwrap();
            s.set_vertex_property_path(a, &path("meta.tags"), "x".into())
                .unwrap();
        }
        "label writes of added elements" => {
            let a = v(s, "a");
            s.add_vertex_label(a, "x".into()).unwrap();
            s.remove_vertex_label(a, "person").unwrap();
            s.set_edge_label(e(s, "ab"), "met".into()).unwrap();
            s.set_edge_label(e(s, "aa"), "self".into()).unwrap();
        }
        "set the schema" => s
            .set_schema(GraphSchema::new().with_mode(SchemaMode::Open))
            .unwrap(),
        "catalog writes" => {
            use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
            s.set_definition(Definition::query(
                "q1",
                QueryDefinition::new("fn q1() { 1 }"),
            ))
            .unwrap();
            s.set_definition(Definition::query(
                "mine",
                QueryDefinition::new("fn mine() { 2 }"),
            ))
            .unwrap();
            assert!(
                s.remove_definition(DefinitionKind::QUERY, "people")
                    .unwrap()
            );
            assert!(
                s.set_definitions(vec![
                    Definition::query("q2", QueryDefinition::new("fn q2() { 1 }")),
                    Definition::query("not valid", QueryDefinition::new("fn x() {}")),
                ])
                .is_err()
            );
        }
        "several writes to one base vertex, then its drop" => {
            let marko = v(s, "1");
            s.set_vertex_property(marko, "age".into(), 31.into())
                .unwrap();
            s.add_vertex_label(marko, "x".into()).unwrap();
            s.drop_vertex_property("name", marko).unwrap();
            let n = s
                .insert_vertex(Some("n2".into()), vec!["n".into()], PropertyMap::default())
                .unwrap();
            s.insert_edge(Some("mn".into()), None, marko, n, PropertyMap::default())
                .unwrap();
            s.drop_vertex(marko).unwrap();
        }
        "a dropped base id reused, written, dropped again" => {
            s.drop_vertex(v(s, "2")).unwrap();
            let n = s
                .insert_vertex(
                    Some("2".into()),
                    vec!["person".into()],
                    PropertyMap::default(),
                )
                .unwrap();
            s.set_vertex_property(n, "k".into(), 1.into()).unwrap();
            let a = v(s, "a");
            s.insert_edge(Some("2a".into()), None, n, a, PropertyMap::default())
                .unwrap();
            s.drop_vertex(n).unwrap();
            s.drop_vertex(a).unwrap();
        }
        "an added vertex dropped and its id reused" => {
            s.drop_vertex(v(s, "b")).unwrap();
            s.insert_vertex(Some("b".into()), vec!["y".into()], PropertyMap::default())
                .unwrap();
        }
        "writes undone by a savepoint" => {
            s.set_vertex_property(v(s, "2"), "age".into(), 1.into())
                .unwrap();
            let inner = s.begin_unit().unwrap();
            s.set_vertex_property(v(s, "2"), "age".into(), 2.into())
                .unwrap();
            s.drop_vertex(v(s, "1")).unwrap();
            s.set_edge_label(e(s, "4"), "y".into()).unwrap();
            s.rollback_unit(inner, &RollbackReason::failed(&"inner"))
                .unwrap();
            s.set_vertex_property(v(s, "2"), "age".into(), 3.into())
                .unwrap();
        }
        "an element added and written in the unit, then dropped" => {
            let n = s
                .insert_vertex(Some("n3".into()), vec!["n".into()], PropertyMap::default())
                .unwrap();
            s.set_vertex_property(n, "k".into(), 1.into()).unwrap();
            s.add_vertex_label(n, "m".into()).unwrap();
            let marko = v(s, "1");
            let edge = s
                .insert_edge(Some("nm".into()), Some("l".into()), n, marko, props(&[]))
                .unwrap();
            s.set_edge_property(edge, "w".into(), 1.into()).unwrap();
            s.drop_vertex(n).unwrap();
        }
        other => panic!("unknown write kind {other}"),
    }
}

/// Runs `kind` in one unit committed under a [`Recorder`] and returns the committed change
/// set.
fn committed_change_set<S: GraphStorage>(s: &mut S, kind: &str) -> ChangeSet {
    let recorder = Recorder::on(s);
    let seq = s.last_commit_seq().unwrap();
    let mark = s.begin_unit().unwrap();
    apply_capture_kind(s, kind);
    s.commit_unit(mark).unwrap();
    assert_eq!(s.last_commit_seq().unwrap(), seq + 1, "{kind}: one commit");
    let accepted = recorder.accepted.lock().unwrap();
    assert_eq!(
        recorder.events(),
        [format!("before:{}", accepted[0].len()), "after".to_string()],
        "{kind}"
    );
    assert_eq!(accepted.len(), 1, "{kind}");
    assert_eq!(accepted[0].commit_seq(), seq + 1, "{kind}");
    accepted[0].clone()
}

/// For every write kind on a layer and on a `TraversalGraph` holding the same data: the
/// committed change sets have the same mutations in the same order (base and added elements,
/// drops with their cascades and the image a dropped element had, identical values, labels,
/// schema, catalog, savepoints) and the same auto-id sequences after the unit; a dry run of
/// the same writes returns the same mutations, is no commit and leaves everything as it was;
/// and each side's change set applied to a fresh copy of the other side gives the other side's
/// state.
#[test]
fn change_sets_match_a_traversal_graph_for_every_write_kind() {
    let layer = prepared_layer();
    let reference = prepared_reference();
    assert_eq!(
        canonical(&layer),
        canonical(&reference),
        "the two starting points hold the same data"
    );
    let mut differences = Vec::new();
    for kind in CAPTURE_KINDS {
        let mut layer = prepared_layer();
        let mut reference = prepared_reference();
        let on_layer = committed_change_set(&mut layer, kind);
        let on_reference = committed_change_set(&mut reference, kind);
        assert!(
            !on_reference.is_empty(),
            "{kind}: the writes change something"
        );
        if on_layer.mutations() != on_reference.mutations() {
            differences.push(format!(
                "{kind}\n  TraversalGraph: {:#?}\n  LayeredGraph:   {:#?}",
                on_reference.mutations(),
                on_layer.mutations()
            ));
            continue;
        }
        assert_eq!(
            (on_layer.node_sequence(), on_layer.edge_sequence()),
            (on_reference.node_sequence(), on_reference.edge_sequence()),
            "{kind}: the auto-id sequences after the unit"
        );
        assert_eq!(
            canonical(&layer),
            canonical(&reference),
            "{kind}: the states"
        );

        // The same writes as a dry run: the same change set, nothing left, no commit.
        let mut dry = prepared_layer();
        let before = canonical(&dry);
        let seq = dry.last_commit_seq().unwrap();
        let recorder = Recorder::on(&mut dry);
        let mark = dry.begin_unit().unwrap();
        apply_capture_kind(&mut dry, kind);
        let discarded = dry.discard_unit(mark).unwrap();
        assert_eq!(
            discarded.mutations(),
            on_layer.mutations(),
            "{kind}: dry run"
        );
        assert_eq!(
            (discarded.commit_seq(), discarded.committed_at()),
            (0, 0),
            "{kind}: a dry run is never committed"
        );
        assert_eq!(canonical(&dry), before, "{kind}: a dry run leaves nothing");
        assert_eq!(dry.last_commit_seq().unwrap(), seq, "{kind}");
        assert!(
            recorder.events().is_empty(),
            "{kind}: no hook hears a dry run"
        );

        // Across: the reference's change set applied to a fresh layer, and vice versa.
        let mut applied = prepared_layer();
        applied.apply_change_set(&on_reference).unwrap();
        assert_eq!(
            canonical(&applied),
            canonical(&reference),
            "{kind}: a TraversalGraph's change set applied to a layer"
        );
        let mut applied = prepared_reference();
        applied.apply_change_set(&on_layer).unwrap();
        assert_eq!(
            canonical(&applied),
            canonical(&layer),
            "{kind}: a layer's change set applied to a TraversalGraph"
        );
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// A veto in `before_commit` rolls the unit back through the layer's undo log (prepared state
/// restored, no commit counted, `CommitRejected` naming the hook) and every hook hears
/// `after_rollback` with the veto; a panic in `before_commit` rolls back too and the panic
/// continues; the next commit works. The same events as on a `TraversalGraph`.
#[test]
fn a_veto_or_a_panicking_hook_rolls_the_unit_back() {
    fn run<S: GraphStorage>(s: &mut S) -> Vec<String> {
        let recorder = Recorder::on(s);
        let before = canonical(s);
        let seq = s.last_commit_seq().unwrap();
        *recorder.veto.lock().unwrap() = true;
        let mark = s.begin_unit().unwrap();
        apply_capture_kind(s, "several writes to one base vertex, then its drop");
        apply_capture_kind(s, "set a property of a clean base edge");
        let vetoed = s.commit_unit(mark).unwrap_err();
        assert!(
            matches!(&vetoed, GraphError::CommitRejected { hook, .. } if hook == "recorder"),
            "{vetoed:?}"
        );
        assert_eq!(canonical(s), before, "the veto rolled everything back");
        assert_eq!(s.last_commit_seq().unwrap(), seq);
        assert_eq!(s.unit_started_at().unwrap(), None, "the unit is closed");
        *recorder.veto.lock().unwrap() = false;

        *recorder.panic.lock().unwrap() = true;
        let mark = s.begin_unit().unwrap();
        apply_capture_kind(s, "drop a clean base vertex");
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = s.commit_unit(mark);
        }));
        assert!(panicked.is_err(), "the panic continues");
        assert_eq!(canonical(s), before, "the panic rolled everything back");
        assert_eq!(s.last_commit_seq().unwrap(), seq);
        *recorder.panic.lock().unwrap() = false;

        let mark = s.begin_unit().unwrap();
        apply_capture_kind(s, "drop a clean base vertex");
        s.commit_unit(mark).unwrap();
        assert_eq!(s.last_commit_seq().unwrap(), seq + 1);
        recorder.events()
    }
    let on_layer = run(&mut prepared_layer());
    let on_reference = run(&mut prepared_reference());
    assert_eq!(on_layer, on_reference);
    assert_eq!(on_layer[1], "rollback:vetoed by recorder");
    assert!(
        on_layer[3].starts_with("rollback:failed: a panic"),
        "{on_layer:?}"
    );
}

/// `after_rollback` carries the reason; it is sent for an outermost unit that recorded
/// anything, also when a savepoint undid it all since (the `changed` flag), and only an
/// identical-value write (recorded, as on a `TraversalGraph`); never for an empty unit, a
/// nested rollback, a dry run or a read-only unit; a schema change refused outside a unit
/// sends nothing either (nothing was recorded). The same events as on a `TraversalGraph`.
#[test]
fn rollbacks_reach_the_hooks_with_their_reason() {
    fn run<S: GraphStorage>(s: &mut S) -> Vec<String> {
        let recorder = Recorder::on(s);
        let mark = s.begin_unit().unwrap();
        s.set_vertex_property(vertex_of(s, "2"), "age".into(), 1.into())
            .unwrap();
        s.rollback_unit(mark, &RollbackReason::failed(&"boom"))
            .unwrap();
        let mark = s.begin_unit().unwrap();
        let inner = s.begin_unit().unwrap();
        s.drop_vertex(vertex_of(s, "1")).unwrap();
        s.rollback_unit(inner, &RollbackReason::failed(&"inner"))
            .unwrap();
        recorder.log("after the savepoint".into());
        s.rollback_unit(mark, &RollbackReason::failed(&"all undone already"))
            .unwrap();
        let mark = s.begin_unit().unwrap();
        let _ = s.vertex_count().unwrap();
        s.rollback_unit(mark, &RollbackReason::failed(&"read only"))
            .unwrap();
        let mark = s.begin_unit().unwrap();
        s.set_vertex_property(vertex_of(s, "2"), "age".into(), 27.into())
            .unwrap();
        s.rollback_unit(mark, &RollbackReason::failed(&"identical value"))
            .unwrap();
        let mark = s.begin_unit().unwrap();
        s.set_vertex_property(vertex_of(s, "2"), "age".into(), 5.into())
            .unwrap();
        let _ = s.discard_unit(mark).unwrap();
        let closed = GraphSchema::new().with_mode(SchemaMode::Closed);
        assert!(s.set_schema(closed).is_err());
        recorder.events()
    }
    let on_layer = run(&mut prepared_layer());
    let on_reference = run(&mut prepared_reference());
    assert_eq!(on_layer, on_reference);
    assert_eq!(
        on_layer,
        [
            "rollback:failed: boom",
            "after the savepoint",
            "rollback:failed: all undone already",
            "rollback:failed: identical value",
        ]
    );
}

/// The metadata of `transaction_with` reaches the hooks (outer unit wins), empty units and
/// read-only units emit nothing, a sequence-only commit (`raise_id_sequences`) reaches the
/// hooks without mutations, and an identical-value write is a commit: all as on a
/// `TraversalGraph`.
#[test]
fn metadata_empty_and_sequence_only_commits_match_a_traversal_graph() {
    fn run<S: GraphStorage>(s: &mut S) -> Vec<String> {
        let recorder = Recorder::on(s);
        let start = s.last_commit_seq().unwrap();
        let outer = CommitMetadata::new()
            .with_principal("ann")
            .with_attribute("why", "outer");
        let kept: Result<(), GraphError> = s.transaction_with(outer, |s| {
            let inner = CommitMetadata::new()
                .with_principal("bob")
                .with_attribute("why", "inner")
                .with_attribute("extra", 1);
            s.transaction_with(inner, |s| {
                s.set_vertex_property(vertex_of(s, "2"), "age".into(), 1.into())
            })
        });
        kept.unwrap();
        let metadata = recorder.accepted.lock().unwrap()[0].metadata().clone();
        recorder.log(format!(
            "metadata {:?} {:?}",
            metadata.principal(),
            metadata.attributes()
        ));
        let mark = s.begin_unit().unwrap();
        s.commit_unit(mark).unwrap();
        let mark = s.begin_unit().unwrap();
        s.raise_id_sequences(1000, 2000).unwrap();
        s.commit_unit(mark).unwrap();
        let sequence_only = recorder.accepted.lock().unwrap()[1].clone();
        recorder.log(format!(
            "sequence-only {} {} {}",
            sequence_only.len(),
            sequence_only.node_sequence(),
            sequence_only.edge_sequence()
        ));
        let mark = s.begin_unit().unwrap();
        s.set_vertex_property(vertex_of(s, "4"), "age".into(), 32.into())
            .unwrap();
        s.commit_unit(mark).unwrap();
        recorder.log(format!("commits {}", s.last_commit_seq().unwrap() - start));
        let mut events = recorder.events();
        events.extend(recorder.mutations().iter().map(|m| format!("{m:?}")));
        events
    }
    let on_layer = run(&mut prepared_layer());
    let on_reference = run(&mut prepared_reference());
    assert_eq!(on_layer, on_reference);
    assert!(
        on_layer.contains(&"commits 3".to_string()),
        "the identical-value write is a commit: {on_layer:?}"
    );
}

/// Commit times come from the layer's instance clock: never earlier than the base's last
/// commit, increasing, shared with `commit_clock()`; the next unit never starts before the
/// last commit; a dry run takes no time.
#[test]
fn commit_times_follow_the_layer_clock() {
    let mut base = TraversalGraph::tinkerpop_modern();
    let base_recorder = Recorder::on(&mut base);
    let mark = base.begin_unit().unwrap();
    let marko = vertex_of(&base, "1");
    base.set_vertex_property(marko, "age".into(), 40.into())
        .unwrap();
    base.commit_unit(mark).unwrap();
    let base_time = base_recorder.accepted.lock().unwrap()[0].committed_at();
    assert!(base_time > 0);

    let mut layer = LayeredGraph::over(Arc::new(base)).unwrap();
    let recorder = Recorder::on(&mut layer);
    let clock = layer.commit_clock().unwrap().expect("a layer has a clock");
    let mut times = Vec::new();
    for age in [1, 2, 3] {
        let mark = layer.begin_unit().unwrap();
        let started = layer.unit_started_at().unwrap().unwrap();
        if let Some(last) = times.last() {
            assert!(
                started >= i128::from(*last) * 1000,
                "a unit never starts before the last commit"
            );
        }
        layer
            .set_vertex_property(v(&layer, "2"), "age".into(), age.into())
            .unwrap();
        layer.commit_unit(mark).unwrap();
        let committed = recorder
            .accepted
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .committed_at();
        times.push(committed);
        assert!(
            clock.floor() >= committed,
            "the clock handed out is the one commits use"
        );
    }
    assert!(
        times[0] >= base_time,
        "never earlier than the base's last commit"
    );
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
    assert!(layer.next_unit_started_at().unwrap().unwrap() >= i128::from(times[2]) * 1000);
}

/// The layer's capture on a base with coded and compressed values reports logical values, as
/// a `TraversalGraph` with the same data and rules does.
#[test]
fn change_sets_over_a_compressed_base_hold_logical_values() {
    use graphersal::catalog::{Codec, CompressionDefinition};
    use graphersal::storage::ElementKind;
    fn graph() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        let long = "a long note that compresses well ".repeat(20);
        let meta = |i: i64| {
            ElementProperty::Object(Box::new(props(&[
                ("file", "src/some/rather/long/path/file.rs".into()),
                ("n", i.into()),
            ])))
        };
        let a = graph
            .insert_vertex(
                Some("a".into()),
                vec!["n".into()],
                props(&[
                    ("note", long.as_str().into()),
                    ("kind", "alpha".into()),
                    ("meta", meta(1)),
                    ("k", 1.into()),
                ]),
            )
            .unwrap();
        let b = graph
            .insert_vertex(
                Some("b".into()),
                vec!["n".into()],
                props(&[
                    ("note", long.as_str().into()),
                    ("kind", "alpha".into()),
                    ("meta", meta(2)),
                ]),
            )
            .unwrap();
        graph
            .insert_edge(
                Some("e".into()),
                Some("l".into()),
                a,
                b,
                props(&[("site", "here".into())]),
            )
            .unwrap();
        let dedup = |element, label, key| {
            CompressionDefinition::new(element, label, key)
                .unwrap()
                .with_codec(Codec::Dedup)
        };
        let mut g = graph.traversal_mut();
        g.define_compression("kind", dedup(ElementKind::Vertex, "n", "kind"))
            .unwrap();
        g.define_compression("file", dedup(ElementKind::Vertex, "n", "meta.file"))
            .unwrap();
        g.define_compression("site", dedup(ElementKind::Edge, "l", "site"))
            .unwrap();
        g.define_compression(
            "note",
            CompressionDefinition::new(ElementKind::Vertex, "n", "note")
                .unwrap()
                .with_dictionary(false),
        )
        .unwrap();
        drop(g);
        graph
    }
    fn run<S: GraphStorage>(s: &mut S) -> Vec<Vec<Mutation>> {
        let recorder = Recorder::on(s);
        let mark = s.begin_unit().unwrap();
        s.set_vertex_property(vertex_of(s, "a"), "k".into(), 2.into())
            .unwrap();
        s.set_vertex_property(vertex_of(s, "a"), "note".into(), "short".into())
            .unwrap();
        s.set_vertex_property(vertex_of(s, "b"), "kind".into(), "alpha".into())
            .unwrap();
        s.set_vertex_property_path(vertex_of(s, "b"), &path("meta.n"), 3.into())
            .unwrap();
        s.set_edge_property(edge_of(s, "e"), "w".into(), 1.into())
            .unwrap();
        s.drop_vertex(vertex_of(s, "b")).unwrap();
        s.commit_unit(mark).unwrap();
        recorder.mutations()
    }
    let mut reference = graph();
    let mut layer = LayeredGraph::over(Arc::new(graph())).unwrap();
    let on_reference = run(&mut reference);
    assert_eq!(run(&mut layer), on_reference);
    let text = format!("{on_reference:?}");
    assert!(
        text.contains("a long note") && text.contains("long/path/file.rs"),
        "{text}"
    );
}

/// Each script as a dry run (`eval_value_dry_run`) on the plain modern graph and on a layer
/// over it: the same result, the same change set, no commit, the state unchanged. Where the
/// script drops elements, the order of the change set may differ only as the plan's order
/// class allows (P4: label-bucket order after drops), checked by comparing it sorted.
#[cfg(feature = "script")]
#[test]
fn rhai_dry_runs_match_the_plain_graph() {
    let scripts: Vec<&str> = RHAI_CREATES
        .iter()
        .chain(RHAI_CROSS_EDGES)
        .chain(RHAI_DROPS)
        .chain(RHAI_DROP_THEN_ADD)
        .chain(RHAI_BASE_WRITES)
        .chain(RHAI_BASE_LABEL_REMOVALS)
        .chain(RHAI_FAILING_WRITES)
        .copied()
        .collect();
    fn dry<S>(
        graph: Arc<Graph<S>>,
        script: &str,
    ) -> (
        Result<(String, Vec<Mutation>), String>,
        u64,
        Result<String, String>,
    )
    where
        S: GraphStorage + Default + Send + Sync + 'static,
    {
        // Unordered: an undone drop stores elements again in another list order (on the plain
        // graph; the layer puts base elements back where the base holds them).
        let before = render(Arc::clone(&graph), UNORDERED_STATE_SCRIPT);
        let seq = graph.read().last_commit_seq().unwrap();
        let result = crate::script_host::eval_value_dry_run(
            Arc::clone(&graph),
            &format!("({{ {script} }}).to_string()"),
            HashMap::new(),
            &crate::script_host::ScriptLimits::default(),
        )
        .map(|(value, changes)| (value.to_string(), changes.mutations().to_vec()))
        .map_err(|err| err.to_string());
        let after = render(Arc::clone(&graph), UNORDERED_STATE_SCRIPT);
        assert_eq!(after, before, "{script}: a dry run leaves nothing");
        assert_eq!(graph.read().last_commit_seq().unwrap(), seq, "{script}");
        (result, seq, after)
    }
    let sorted = |mutations: &[Mutation]| {
        let mut texts: Vec<String> = mutations.iter().map(|m| format!("{m:?}")).collect();
        texts.sort();
        texts
    };
    let mut differences = Vec::new();
    let mut order_only = Vec::new();
    let mut with_changes = 0;
    for script in &scripts {
        let expected = dry(plain_graph(), script).0;
        let actual = dry(layered_graph(), script).0;
        if let Ok((_, changes)) = &expected {
            with_changes += usize::from(!changes.is_empty());
        }
        if expected == actual {
            continue;
        }
        if let (Ok((e_value, e_changes)), Ok((a_value, a_changes))) = (&expected, &actual)
            && script.contains("drop")
            && sorted(e_changes) == sorted(a_changes)
            && (e_value == a_value || script.contains("hasLabel"))
        {
            order_only.push(*script);
            continue;
        }
        differences.push(format!(
            "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}"
        ));
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    assert!(
        with_changes >= 40,
        "the scripts really write: {with_changes}"
    );
    println!(
        "order-only differences: {}\n{}",
        order_only.len(),
        order_only.join("\n")
    );
}

/// The same scripts with a hook registered on `Graph<LayeredGraph>` and on the plain graph,
/// each traversal a unit of its own: the hooks see the same commits with the same mutations
/// (failing traversals none), and the same rollback events.
#[cfg(feature = "script")]
#[test]
fn rhai_captured_commits_match_the_plain_graph() {
    let scripts: Vec<&str> = RHAI_CREATES
        .iter()
        .chain(RHAI_CROSS_EDGES)
        .chain(RHAI_DROPS)
        .chain(RHAI_BASE_WRITES)
        .chain(RHAI_FAILING_WRITES)
        .copied()
        .collect();
    fn captured<S>(
        graph: Arc<Graph<S>>,
        script: &str,
    ) -> (Result<String, String>, Vec<String>, Vec<Vec<Mutation>>)
    where
        S: GraphStorage + Default + Send + Sync + 'static,
    {
        let recorder = Recorder::default();
        graph
            .write()
            .register_commit_hook(Box::new(recorder.clone()))
            .unwrap();
        let result = render(Arc::clone(&graph), script);
        (result, recorder.events(), recorder.mutations())
    }
    let mut differences = Vec::new();
    let mut commits = 0;
    let mut compare = |script: &str,
                       expected: (Result<String, String>, Vec<String>, Vec<Vec<Mutation>>),
                       actual| {
        commits += expected.2.len();
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  LayeredGraph:   {actual:?}"
            ));
        }
    };
    for script in &scripts {
        compare(
            script,
            captured(plain_graph(), script),
            captured(layered_graph(), script),
        );
    }
    // Schema and catalog scripts on the product base (with its saved query and jobs).
    for script in RHAI_SCHEMA_SCRIPTS {
        let plain = Arc::new(Graph::new(product_base()));
        let layered = Arc::new(Graph::new(
            LayeredGraph::over(Arc::new(product_base())).unwrap(),
        ));
        compare(script, captured(plain, script), captured(layered, script));
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
    assert!(commits >= 60, "the hooks really saw commits: {commits}");
}

/// Adds `n` vertices labelled `tmp` with ids `<prefix><i>` to `g`.
fn add_tmp_vertices(g: &mut LayeredGraph, prefix: &str, n: usize) -> Result<(), GraphError> {
    for i in 0..n {
        g.add_vertex(
            Some(format!("{prefix}{i}").into()),
            vec!["tmp".into()],
            Default::default(),
        )?;
    }
    Ok(())
}

/// The memory figures of a layer (S1.11): its own footprint as the total, the shared base
/// beside it; writes grow the layer, never the base's figure, and `shrink_to_fit` releases the
/// delta's spare capacity, also through `Graph` and the DSL.
#[test]
fn a_layer_reports_its_own_footprint_and_the_shared_base() {
    let base = modern_base();
    let base_total = base.memory_usage().unwrap().unwrap().total_bytes();
    let layer = LayeredGraph::over(Arc::clone(&base)).unwrap();
    let untouched = layer.memory_usage().unwrap().unwrap();
    assert_eq!(untouched.shared_bytes, base_total);
    assert!(untouched.total_bytes() > 0 && untouched.total_bytes() < base_total);
    let graph = Graph::new(layer);
    for i in 0..1000i64 {
        GraphTraversalSource::new(&mut *graph.write())
            .add_v("p")
            .property("i", i)
            .iterate()
            .unwrap();
    }
    let grown = graph.read().memory_usage().unwrap().unwrap();
    assert_eq!(grown.shared_bytes, base_total, "the base is unchanged");
    assert!(grown.total_bytes() > untouched.total_bytes() + 1000 * 64);
    let report = graph.shrink_to_fit().unwrap();
    assert!(report.released_bytes().unwrap() > 0);
    assert_eq!(graph.shrink_to_fit().unwrap().released_bytes(), Some(0));
    assert_eq!(graph.read().vertex_count().unwrap(), 1006);
    assert_eq!(TraversalGraph::vertex_count(&base), 6);
}

/// The DSL's `g.memory_usage()` on a layer: the total is the layer's own, `shared_bytes` the
/// base's.
#[cfg(feature = "script")]
#[test]
fn the_dsl_memory_usage_of_a_layer_names_the_shared_base() {
    let base = modern_base();
    let base_total = base.memory_usage().unwrap().unwrap().total_bytes();
    let graph = Arc::new(Graph::new(LayeredGraph::over(base).unwrap()));
    let shared = render(
        Arc::clone(&graph),
        "let m = g.memory_usage(); [m.shared_bytes, m.total_bytes < m.shared_bytes]",
    )
    .unwrap();
    assert_eq!(shared, format!("[{base_total}, true]"));
}

/// The memory budget at a layer's commit (S1.11), as `TraversalGraph`'s
/// (`memory_budget_tests::a_transaction_over_the_budget_rolls_back_at_its_commit`): a unit that
/// grows the layer over the ceiling rolls back with `ResourceLimitExceeded` naming `commit`; a
/// smaller one commits; a unit that only removes commits over the ceiling. The ceiling is
/// measured on the layer's OWN footprint: the shared base (larger than the ceiling here) does
/// not count.
#[test]
fn a_layer_unit_over_the_memory_budget_rolls_back_at_its_commit() {
    use graphersal::exec::memory_limit_scope;
    let mut layer = modern_layer();
    let stats = layer.memory_usage().unwrap().unwrap();
    let limit = stats.total_bytes() + 1_000;
    assert!(
        stats.shared_bytes > limit,
        "the base alone is over the ceiling"
    );
    memory_limit_scope(limit, || {
        let err = layer
            .transaction(|g| add_tmp_vertices(g, "tmp", 100))
            .unwrap_err();
        match &err {
            GraphError::ResourceLimitExceeded {
                operation,
                max: Some(max),
                observed: Some(observed),
                ..
            } => {
                assert_eq!(*operation, "commit");
                assert_eq!(*max, limit);
                assert!(*observed > limit);
            }
            other => panic!("expected ResourceLimitExceeded, got {other:?}"),
        }
        assert_eq!(layer.vertex_count().unwrap(), 6);
        assert_eq!(layer.last_commit_seq().unwrap(), 0);
        // A savepoint is not checked on its own: the outermost commit decides.
        assert!(
            layer
                .transaction(|g| g.transaction(|inner| add_tmp_vertices(inner, "tmp", 100)))
                .is_err()
        );
        assert_eq!(layer.vertex_count().unwrap(), 6);
        layer
            .transaction(|g| add_tmp_vertices(g, "tmp", 3))
            .unwrap();
        assert_eq!(layer.vertex_count().unwrap(), 9);
        assert_eq!(layer.last_commit_seq().unwrap(), 1);
    });
    // Over a ceiling the layer itself exceeds, removing still commits; adding does not.
    memory_limit_scope(1, || {
        assert!(
            layer
                .transaction(|g| add_tmp_vertices(g, "more", 1))
                .is_err()
        );
        let marko = v(&layer, "1");
        layer.transaction(|g| g.drop_vertex(marko)).unwrap();
        assert_eq!(layer.vertex_count().unwrap(), 8);
    });
}
