//! Units of atomicity and the undo log of `TraversalGraph` (`book/src/users_guide/transactions.md`).
//!
//! Storage level: every mutating `GraphStorage` method inside `begin_unit()` is undone by
//! `rollback_unit()`, restoring the structure, the id and label indexes and the counts (the
//! auto-id sequences stay advanced: an id is never reused); nested units are savepoints. Traversal level: every terminal runs its
//! traversal as one unit (implicit auto-commit), so a failing traversal leaves nothing behind,
//! in the Rust API and in the Rhai DSL. Explicit transactions (`TraversalGraph::transaction`,
//! nested = savepoint, closure error and veto roll back everything), dry runs, and the
//! whole-script unit of the Rhai host option `eval_value_atomic`.

use graphersal::prelude::*;
use graphersal::storage::GraphStatistics;
use graphersal::{
    changes::RollbackReason,
    storage::{EdgeHandle, UnitMark, VertexHandle},
};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn props(entries: &[(&str, ElementProperty)]) -> PropertyMap {
    let mut out = PropertyMap::default();
    for (key, value) in entries {
        out.insert(*key, value.clone());
    }
    out
}

fn path(text: &str) -> JsonPath {
    text.parse().expect("valid jpath")
}

/// Everything observable about a graph's content, in a canonical order: vertices
/// `(id, labels, properties in stored order)`, edges `(id, label, source id, target id,
/// properties)`, per-vertex adjacency as id sets, the label counts and the label index lookups.
/// Properties in stored order.
type Props = Vec<(String, ElementProperty)>;
/// `(id, labels, properties)`.
type VertexRow = (String, Vec<String>, Props);
/// `(id, label, source id, target id, properties)`.
type EdgeRow = (String, Option<String>, String, String, Option<Props>);

#[derive(Debug, PartialEq)]
struct Snapshot {
    vertices: Vec<VertexRow>,
    edges: Vec<EdgeRow>,
    adjacency: Vec<(String, Vec<String>, Vec<String>)>,
    vertex_label_counts: Vec<(String, usize)>,
    primary_label_counts: Vec<(String, usize)>,
    edge_label_counts: Vec<(String, usize)>,
    by_label: Vec<(String, Vec<String>)>,
    edges_by_label: Vec<(String, Vec<String>)>,
    counts: (usize, usize),
}

fn entries(element: &PropertyMap) -> Props {
    element
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
    items.sort();
    items
}

fn snapshot(graph: &TraversalGraph) -> Snapshot {
    let vid = |v: VertexHandle| {
        graph
            .vertex_id(v)
            .unwrap()
            .expect("live vertex")
            .to_string()
    };
    let eid = |e: EdgeHandle| graph.edge_id(e).unwrap().expect("live edge").to_string();
    let mut vertices: Vec<_> = GraphStorage::vertices(graph)
        .unwrap()
        .map(|v| {
            (
                vid(v),
                graph
                    .vertex_labels(v)
                    .unwrap()
                    .iter()
                    .map(|l| l.to_string())
                    .collect(),
                entries(graph.vertex_properties(v).unwrap().expect("properties")),
            )
        })
        .collect();
    vertices.sort_by(|a, b| a.0.cmp(&b.0));
    let mut edges: Vec<_> = GraphStorage::edges(graph)
        .unwrap()
        .map(|e| {
            let (from, to) = graph.edge_endpoints(e).unwrap().expect("endpoints");
            (
                eid(e),
                graph.edge_label(e).unwrap().map(str::to_string),
                vid(from),
                vid(to),
                graph.edge_properties(e).unwrap().map(entries),
            )
        })
        .collect();
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    let mut adjacency: Vec<_> = GraphStorage::vertices(graph)
        .unwrap()
        .map(|v| {
            (
                vid(v),
                sorted(graph.out_edges(v).unwrap().map(eid).collect()),
                sorted(graph.in_edges(v).unwrap().map(eid).collect()),
            )
        })
        .collect();
    adjacency.sort();
    // The incrementally kept statistics always equal a full recomputation.
    let stats = graph
        .statistics()
        .unwrap()
        .expect("TraversalGraph keeps statistics");
    assert_eq!(stats, &GraphStatistics::from_storage(graph).unwrap());
    let owned = |counts: Vec<(&str, usize)>| -> Vec<(String, usize)> {
        counts
            .into_iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect()
    };
    let vertex_label_counts = owned(stats.vertex_label_counts());
    let primary_label_counts = owned(stats.primary_vertex_label_counts());
    let edge_label_counts = owned(stats.edge_label_counts());
    let by_label = vertex_label_counts
        .iter()
        .map(|(label, _)| {
            (
                label.clone(),
                sorted(graph.vertices_by_label(label).unwrap().map(vid).collect()),
            )
        })
        .collect();
    let edges_by_label = edge_label_counts
        .iter()
        .map(|(label, _)| {
            (
                label.clone(),
                sorted(graph.edges_by_label(label).unwrap().map(eid).collect()),
            )
        })
        .collect();
    // Every id resolves through the id index to the element carrying it.
    for (id, _, _) in &vertices {
        let handle = graph.get_vertex_by_id(id).unwrap().expect("id index entry");
        assert_eq!(graph.vertex_id(handle).unwrap(), Some(id.as_str()));
    }
    for (id, ..) in &edges {
        let handle = graph.get_edge_by_id(id).unwrap().expect("id index entry");
        assert_eq!(graph.edge_id(handle).unwrap(), Some(id.as_str()));
    }
    Snapshot {
        vertices,
        edges,
        adjacency,
        vertex_label_counts,
        primary_label_counts,
        edge_label_counts,
        by_label,
        edges_by_label,
        counts: (graph.vertex_count(), graph.edge_count()),
    }
}

/// Runs `change` inside a unit, rolls it back and checks the graph is exactly as before.
fn assert_rolled_back(
    mut graph: TraversalGraph,
    change: impl FnOnce(&mut TraversalGraph) -> Result<(), GraphError>,
) -> TraversalGraph {
    let before = snapshot(&graph);
    let mark = graph.begin_unit().unwrap();
    change(&mut graph).expect("the change itself succeeds");
    assert_ne!(snapshot(&graph), before, "the change must be visible");
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(snapshot(&graph), before);
    graph
}

fn v(graph: &TraversalGraph, id: &str) -> VertexHandle {
    graph.get_vertex_by_id(id).unwrap().expect("vertex")
}

fn e(graph: &TraversalGraph, id: &str) -> EdgeHandle {
    graph.get_edge_by_id(id).unwrap().expect("edge")
}

/// The id the next auto-id vertex gets (added and rolled back in a unit of its own).
fn next_vertex_id(graph: &mut TraversalGraph) -> String {
    let mark = graph.begin_unit().unwrap();
    let handle = graph
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    let id = graph.vertex_id(handle).unwrap().unwrap().to_string();
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    id
}

/// The id the next auto-id edge gets.
fn next_edge_id(graph: &mut TraversalGraph) -> String {
    let mark = graph.begin_unit().unwrap();
    let a = v(graph, "1");
    let handle = graph
        .add_edge(None, None, a, a, PropertyMap::default())
        .unwrap();
    let id = graph.edge_id(handle).unwrap().unwrap().to_string();
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    id
}

// ---------------------------------------------------------------------------------------------
// Storage level: one test per mutating method
// ---------------------------------------------------------------------------------------------

#[test]
fn add_vertex_is_rolled_back_but_its_id_is_never_reused() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let next = next_vertex_id(&mut graph);
    let mut graph = assert_rolled_back(graph, |g| {
        g.add_vertex(None, vec!["person".into()], props(&[("name", "x".into())]))?;
        g.add_vertex(
            Some("500".into()),
            vec!["robot".into()],
            PropertyMap::default(),
        )?;
        Ok(())
    });
    assert_ne!(
        next_vertex_id(&mut graph),
        next,
        "an auto id once handed out is never reused"
    );
    assert!(graph.get_vertex_by_id("500").unwrap().is_none());
}

#[test]
fn add_edge_is_rolled_back_but_its_id_is_never_reused() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let next = next_edge_id(&mut graph);
    let mut graph = assert_rolled_back(graph, |g| {
        let (a, b) = (v(g, "1"), v(g, "6"));
        g.add_edge(None, Some("likes".into()), a, b, PropertyMap::default())?;
        g.add_edge(
            Some("900".into()),
            Some("knows".into()),
            b,
            a,
            PropertyMap::default(),
        )?;
        Ok(())
    });
    assert_ne!(
        next_edge_id(&mut graph),
        next,
        "an auto id once handed out is never reused"
    );
}

#[test]
fn drop_vertex_with_edges_is_restored() {
    let graph = TraversalGraph::tinkerpop_modern();
    let old = v(&graph, "1");
    let graph = assert_rolled_back(graph, |g| {
        let marko = v(g, "1");
        g.drop_vertex(marko)
    });
    // The restored vertex is a new element: the handle from before the drop stays stale.
    assert!(!graph.contains_vertex(old).unwrap());
    assert_eq!(graph.vertex_properties(old).unwrap(), None);
    let marko = v(&graph, "1");
    assert_eq!(graph.out_edges(marko).unwrap().count(), 3);
    assert_eq!(
        graph
            .edge_endpoints(e(&graph, "0"))
            .unwrap()
            .map(|(s, _)| s),
        Some(marko)
    );
}

#[test]
fn drop_of_a_vertex_with_a_self_loop_is_restored() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let a = v(&graph, "4");
    graph
        .add_edge(
            Some("loop".into()),
            Some("self".into()),
            a,
            a,
            PropertyMap::default(),
        )
        .unwrap();
    assert_rolled_back(graph, |g| {
        let josh = v(g, "4");
        g.drop_vertex(josh)
    });
}

#[test]
fn drop_edge_is_restored() {
    let graph = TraversalGraph::tinkerpop_modern();
    let old = e(&graph, "2");
    let graph = assert_rolled_back(graph, |g| {
        let edge = e(g, "2");
        g.drop_edge(edge)
    });
    assert!(!graph.contains_edge(old).unwrap());
}

#[test]
fn vertex_property_writes_and_removals_are_restored_in_place() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let marko = v(&graph, "1");
    graph
        .set_vertex_property(marko, "city".into(), "rome".into())
        .unwrap();
    assert_rolled_back(graph, |g| {
        let marko = v(g, "1");
        g.set_vertex_property(marko, "age".into(), 30.into())?;
        g.set_vertex_property(marko, "new".into(), true.into())?;
        // Removing the middle key and rolling back puts it back at its position.
        g.drop_vertex_property("age", marko)?;
        g.drop_vertex_property("absent", marko)?;
        Ok(())
    });
}

#[test]
fn edge_property_writes_and_removals_are_restored() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // An edge created without properties gets its first one inside the unit.
    let (a, b) = (v(&graph, "2"), v(&graph, "3"));
    graph
        .add_edge(Some("bare".into()), None, a, b, PropertyMap::default())
        .unwrap();
    assert_rolled_back(graph, |g| {
        let edge = e(g, "0");
        g.set_edge_property(edge, "weight".into(), 0.9.into())?;
        g.set_edge_property(edge, "since".into(), 2001.into())?;
        g.drop_edge_property("weight", edge)?;
        let bare = e(g, "bare");
        g.set_edge_property(bare, "k".into(), 1.into())?;
        Ok(())
    });
}

#[test]
fn label_changes_are_restored_with_the_label_index() {
    let graph = TraversalGraph::tinkerpop_modern();
    assert_rolled_back(graph, |g| {
        let marko = v(g, "1");
        g.add_vertex_label(marko, "admin".into())?;
        g.remove_vertex_label(marko, "person")?;
        let lop = v(g, "3");
        g.add_vertex_label(lop, "person".into())?;
        Ok(())
    });
}

#[test]
fn jpath_writes_and_removals_are_restored_without_and_with_a_schema() {
    for mode in [SchemaMode::None, SchemaMode::Open] {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let marko = v(&graph, "1");
        let mut meta = PropertyMap::default();
        meta.insert("k", 1.into());
        meta.insert("tags", ElementProperty::Array(vec!["a".into(), "b".into()]));
        graph
            .set_vertex_property(
                marko,
                "meta".into(),
                ElementProperty::Object(Box::new(meta)),
            )
            .unwrap();
        let edge = e(&graph, "0");
        graph
            .set_edge_property_path(edge, &path("info.x"), 1.into())
            .unwrap();
        graph
            .set_schema(GraphSchema::new().with_mode(mode))
            .unwrap();
        assert_rolled_back(graph, |g| {
            let marko = v(g, "1");
            g.set_vertex_property_path(marko, &path("meta.k"), 2.into())?;
            g.set_vertex_property_path(marko, &path("meta.tags[2]"), "c".into())?;
            g.remove_vertex_property_path(marko, &path("meta.tags[0]"))?;
            g.set_vertex_property_path(marko, &path("fresh.a"), 1.into())?;
            let edge = e(g, "0");
            g.set_edge_property_path(edge, &path("info.y"), 2.into())?;
            g.remove_edge_property_path(edge, &path("info.x"))?;
            Ok(())
        });
    }
}

#[test]
fn schema_change_is_rolled_back() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = graph.get_schema().unwrap();
    let mark = graph.begin_unit().unwrap();
    GraphStorage::set_schema(&mut graph, GraphSchema::new().with_mode(SchemaMode::Open)).unwrap();
    graph
        .add_vertex(Some("x".into()), vec!["n".into()], PropertyMap::default())
        .unwrap();
    assert_eq!(
        graph.get_schema().unwrap().map(|s| s.mode),
        Some(SchemaMode::Open)
    );
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(graph.get_schema().unwrap(), before);
    assert!(graph.get_vertex_by_id("x").unwrap().is_none());
}

#[test]
fn a_mixed_unit_is_rolled_back_completely() {
    let graph = TraversalGraph::tinkerpop_modern();
    assert_rolled_back(graph, |g| {
        // Add, connect, write, drop the new vertex again, drop an old one, reuse a freed id.
        let n = g.add_vertex(Some("n".into()), vec!["new".into()], PropertyMap::default())?;
        let marko = v(g, "1");
        g.add_edge(
            Some("ne".into()),
            Some("knows".into()),
            n,
            marko,
            PropertyMap::default(),
        )?;
        g.set_vertex_property(n, "p".into(), 1.into())?;
        g.drop_vertex(n)?;
        let josh = v(g, "4");
        g.drop_vertex(josh)?;
        g.add_vertex(
            Some("4".into()),
            vec!["impostor".into()],
            PropertyMap::default(),
        )?;
        g.add_vertex_label(marko, "x".into())?;
        let peter = v(g, "6");
        g.set_vertex_property(peter, "age".into(), 99.into())?;
        Ok(())
    });
}

#[test]
fn nested_units_are_savepoints() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let outer = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("kept".into()), vec![], PropertyMap::default())
        .unwrap();
    let after_outer = snapshot(&graph);
    let inner = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("undone".into()), vec![], PropertyMap::default())
        .unwrap();
    let marko = v(&graph, "1");
    graph.drop_vertex(marko).unwrap();
    graph
        .rollback_unit(inner, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(
        snapshot(&graph),
        after_outer,
        "only the inner unit is undone"
    );
    // A committed inner unit is still undone by a rollback of its outer unit.
    let inner = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("inner".into()), vec![], PropertyMap::default())
        .unwrap();
    graph.commit_unit(inner).expect("commit");
    graph.commit_unit(outer).expect("commit");
    assert!(graph.get_vertex_by_id("kept").unwrap().is_some());
    assert!(graph.get_vertex_by_id("inner").unwrap().is_some());
    assert!(graph.get_vertex_by_id("undone").unwrap().is_none());

    let before = snapshot(&graph);
    let outer = graph.begin_unit().unwrap();
    let inner = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("gone".into()), vec![], PropertyMap::default())
        .unwrap();
    graph.commit_unit(inner).expect("commit");
    graph
        .rollback_unit(outer, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(snapshot(&graph), before);
}

#[test]
fn a_committed_unit_is_final() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let mark = graph.begin_unit().unwrap();
    graph
        .add_vertex(Some("x".into()), vec![], PropertyMap::default())
        .unwrap();
    graph.commit_unit(mark).expect("commit");
    // A later unit's rollback does not reach behind the commit.
    let mark = graph.begin_unit().unwrap();
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert!(graph.get_vertex_by_id("x").unwrap().is_some());
}

#[test]
fn units_on_a_read_only_reference_are_no_ops() {
    let graph = TraversalGraph::tinkerpop_modern();
    let mut storage = &graph;
    let mark = storage.begin_unit().unwrap();
    assert_eq!(mark, UnitMark::default());
    storage
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(graph.vertex_count(), 6);
}

#[test]
fn units_are_forwarded_by_box_and_arc() {
    let mut boxed: Box<TraversalGraph> = Box::new(TraversalGraph::tinkerpop_modern());
    let mark = boxed.begin_unit().unwrap();
    GraphStorage::add_vertex(&mut boxed, Some("b".into()), vec![], PropertyMap::default()).unwrap();
    boxed
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert!(boxed.get_vertex_by_id("b").unwrap().is_none());

    let mut shared = std::sync::Arc::new(TraversalGraph::tinkerpop_modern());
    let mark = shared.begin_unit().unwrap();
    GraphStorage::add_vertex(
        &mut shared,
        Some("a".into()),
        vec![],
        PropertyMap::default(),
    )
    .unwrap();
    shared
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert!(shared.get_vertex_by_id("a").unwrap().is_none());
}

// ---------------------------------------------------------------------------------------------
// Traversal level: implicit auto-commit
// ---------------------------------------------------------------------------------------------

/// A traversal that drops marko (with his three edges), writes a property and adds a vertex, then
/// fails in a cast.
fn failing_traversal(graph: &mut TraversalGraph) -> GraphTraversalSource<'_, &mut TraversalGraph> {
    graph
        .traversal_mut()
        .inject(vec![ElementProperty::from(1)])
        .side_effect_t(__::v("1").drop())
        .side_effect_t(__::v("2").property("age", 99))
        .side_effect_t(__::add_v("ghost"))
        .constant("x")
        .as_number(GType::LONG)
}

#[test]
fn a_failing_traversal_leaves_nothing_behind() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = snapshot(&graph);
    assert!(failing_traversal(&mut graph).to_list().is_err());
    assert_eq!(snapshot(&graph), before);
    assert!(failing_traversal(&mut graph).next().is_err());
    assert_eq!(snapshot(&graph), before);
    assert!(failing_traversal(&mut graph).profile().is_err());
    assert_eq!(snapshot(&graph), before);
    assert!(failing_traversal(&mut graph).to_table().is_err());
    assert_eq!(snapshot(&graph), before);
    assert!(failing_traversal(&mut graph).iterate().is_err());
    assert_eq!(snapshot(&graph), before);
    // execute(): the error and the partial profile are kept, the results are not applied.
    let exec =
        failing_traversal(&mut graph).execute_with(ExecOptions::profile(ProfileType::none()));
    assert!(exec.error().is_some());
    assert!(exec.results().is_empty());
    assert!(exec.profile().is_some());
    assert_eq!(snapshot(&graph), before);
    // Auto ids handed out by the failed traversal are not reused.
    let next = next_vertex_id(&mut graph);
    assert!(failing_traversal(&mut graph).to_list().is_err());
    assert_ne!(next_vertex_id(&mut graph), next);
}

#[test]
fn a_successful_traversal_is_committed() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph.traversal_mut().v("1").drop().to_list().unwrap();
    assert!(graph.get_vertex_by_id("1").unwrap().is_none());
    assert_eq!(graph.edge_count(), 3);
}

#[test]
fn a_materialization_error_rolls_back_too() {
    // The run succeeds, then the terminal fails materializing the dropped vertex.
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = snapshot(&graph);
    let result = graph
        .traversal_mut()
        .v("1")
        .side_effect_t(__::drop())
        .to_list();
    assert!(result.is_err(), "{result:?}");
    assert_eq!(snapshot(&graph), before);
}

#[test]
fn a_timeout_rolls_back_the_writes_before_it() {
    let graph = GraphSource::large_110k();
    let err = {
        let mut lock = graph.write();
        lock.traversal_mut()
            .with("evaluationTimeout", 50)
            .v("V_0_0")
            .property("x", 1)
            .side_effect_t(__::add_v("marker"))
            .where_t(
                __::both(None)
                    .both(None)
                    .both(None)
                    .both(None)
                    .both(None)
                    .both(None)
                    .both(None)
                    .has("level", "none"),
            )
            .count()
            .next()
            .expect_err("the where() step exceeds 50 ms")
    };
    assert!(
        matches!(err.root_cause(), TraverserError::Timeout { .. }),
        "{err:?}"
    );
    let lock = graph.read();
    assert_eq!(
        lock.traversal().v("V_0_0").values("x").next().unwrap(),
        None
    );
    assert_eq!(
        lock.traversal()
            .v(None)
            .has_label("marker")
            .count()
            .next()
            .unwrap(),
        Some(0.into())
    );
}

#[test]
fn a_schema_violation_rolls_back_the_whole_traversal() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let mut schema = graph.infer_schema().unwrap();
    schema.mode = SchemaMode::Closed;
    graph.set_schema(schema).unwrap();
    let before = snapshot(&graph);
    // The first property is declared, the second label is not: Closed rejects the new vertex.
    let result = graph
        .traversal_mut()
        .v("1")
        .property("age", 30)
        .add_v("undeclared")
        .to_list();
    assert!(result.is_err());
    assert_eq!(snapshot(&graph), before);
}

// ---------------------------------------------------------------------------------------------
// Rhai: the unit is one traversal
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
mod script {
    use std::sync::Arc;

    use crate::script_host::eval_value;
    use graphersal::prelude::*;

    fn int(graph: &Arc<Graph>, script: &str) -> i64 {
        eval_value(graph.clone(), script)
            .unwrap_or_else(|e| panic!("`{script}` failed: {e}"))
            .as_int()
            .unwrap_or_else(|_| panic!("`{script}` is not an int"))
    }

    const STATE: &str = r#"g.V().count().next() * 1000 + g.E().count().next() * 100
        + g.V().has("touched").count().next() * 10 + g.V("1").outE().count().next()"#;

    #[test]
    fn a_failing_mutating_statement_leaves_nothing() {
        for script in [
            // snake_case and camelCase spellings, a terminal and a bare (rendered) traversal
            r#"g.v().property("touched", true).values("name").as_number(GType::LONG).to_list()"#,
            r#"g.V().property("touched", true).values("name").asNumber(GType.LONG).toList()"#,
            r#"g.inject(1).sideEffect(__.V("1").drop()).constant("x").asNumber(GType.LONG).next()"#,
            r#"g.addV("n").property(T.id, "1").toList()"#,
            r#"g.V("1", "2").addE("knows").to(__.V("nope")).property(T.id, "0").toList()"#,
        ] {
            let graph = Arc::new(GraphSource::tinkerpop_modern());
            let before = int(&graph, STATE);
            assert!(eval_value(graph.clone(), script).is_err(), "{script}");
            assert_eq!(int(&graph, STATE), before, "{script}");
        }
    }

    #[test]
    fn a_bare_failing_traversal_and_execute_leave_nothing() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let before = int(&graph, STATE);
        let _ = crate::script_host::eval(
            graph.clone(),
            r#"g.V().property("touched", true).values("name").asNumber(GType.LONG)"#,
        );
        assert_eq!(int(&graph, STATE), before);
        let error = eval_value(
            graph.clone(),
            r#"let r = g.V().property("touched", true).values("name").asNumber(GType.LONG).execute(); r.results.len() * 10 + if r.error == () { 0 } else { 1 }"#,
        )
        .unwrap()
        .as_int()
        .unwrap();
        assert_eq!(error, 1, "no results, an error");
        assert_eq!(int(&graph, STATE), before);
    }

    #[test]
    fn earlier_statements_of_the_script_stay() {
        // The default unit is one traversal, not the whole script.
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let result = eval_value(
            graph.clone(),
            r#"
            g.V("1").property("touched", true).toList();
            g.V("2").property("touched", true).values("name").asNumber(GType.LONG).toList();
            "#,
        );
        assert!(result.is_err());
        assert_eq!(int(&graph, r#"g.V().has("touched").count().next()"#), 1);
    }
}

// ---------------------------------------------------------------------------------------------
// Explicit transactions (`TraversalGraph::transaction`) and dry runs (`dry_run`)
// ---------------------------------------------------------------------------------------------

mod explicit {
    use std::sync::{Arc, Mutex};

    use graphersal::changes::{ChangeSet, Mutation};
    use graphersal::changes::{CommitHook, CommitMetadata, HookError, RollbackReason};
    use graphersal::prelude::*;

    use super::{next_vertex_id, snapshot};

    /// What a [`Recorder`] saw: `before:<seq>`, `after:<seq>`, `rollback:<vetoed|failed>`, and
    /// every change set handed to `before_commit`.
    #[derive(Default)]
    pub(super) struct Seen {
        pub(super) events: Vec<String>,
        pub(super) change_sets: Vec<ChangeSet>,
        pub(super) veto: bool,
    }

    pub(super) struct Recorder(pub(super) Arc<Mutex<Seen>>);

    #[derive(Debug)]
    struct Refused;

    impl std::fmt::Display for Refused {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("refused")
        }
    }

    impl std::error::Error for Refused {}

    impl CommitHook for Recorder {
        fn name(&self) -> &str {
            "recorder"
        }

        fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
            let mut seen = self.0.lock().unwrap();
            seen.events.push(format!("before:{}", changes.commit_seq()));
            seen.change_sets.push(changes.clone());
            if seen.veto {
                return Err(Box::new(Refused));
            }
            Ok(())
        }

        fn after_commit(&mut self, changes: &ChangeSet) {
            let mut seen = self.0.lock().unwrap();
            seen.events.push(format!("after:{}", changes.commit_seq()));
        }

        fn after_rollback(&mut self, reason: &RollbackReason) {
            let kind = match reason {
                RollbackReason::Vetoed { .. } => "vetoed",
                RollbackReason::Failed { .. } => "failed",
                _ => "other",
            };
            self.0
                .lock()
                .unwrap()
                .events
                .push(format!("rollback:{kind}"));
        }
    }

    /// Registers a recorder on `storage` and returns what it sees.
    pub(super) fn record<S: GraphStorage>(storage: &mut S) -> Arc<Mutex<Seen>> {
        let seen = Arc::new(Mutex::new(Seen::default()));
        storage
            .register_commit_hook(Box::new(Recorder(seen.clone())))
            .expect("hooks are supported");
        seen
    }

    fn take_events(seen: &Arc<Mutex<Seen>>) -> Vec<String> {
        std::mem::take(&mut seen.lock().unwrap().events)
    }

    fn names(graph: &TraversalGraph) -> Vec<String> {
        let mut names: Vec<String> = graph
            .traversal()
            .v(None::<()>)
            .values("name")
            .to_list()
            .unwrap()
            .into_iter()
            .map(|name| match name {
                ElementProperty::String(name) => name.to_string(),
                other => panic!("not a name: {other:?}"),
            })
            .collect();
        names.sort();
        names
    }

    fn has_name(graph: &TraversalGraph, name: &str) -> bool {
        names(graph).iter().any(|n| n == name)
    }

    /// A traversal that adds a vertex, then fails in a cast.
    fn add_then_fail(
        graph: &mut TraversalGraph,
        name: &str,
    ) -> TraverserResult<Vec<ElementProperty>> {
        graph
            .traversal_mut()
            .add_v("person")
            .property("name", name)
            .constant("x")
            .as_number(GType::LONG)
            .to_list()
    }

    fn add(graph: &mut TraversalGraph, name: &str) -> TraverserResult<Vec<ElementProperty>> {
        graph
            .traversal_mut()
            .add_v("person")
            .property("name", name)
            .to_list()
    }

    #[test]
    fn a_failing_traversal_rolls_back_only_to_its_savepoint() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let seen = record(&mut graph);
        let seq = graph.last_commit_seq();
        let outcome: TraverserResult<bool> = graph.transaction(|g| {
            add(g, "ann")?;
            // The closure receives the error and decides to go on.
            let failed = add_then_fail(g, "ghost").is_err();
            add(g, "bob")?;
            Ok(failed)
        });
        assert!(outcome.unwrap());
        assert!(has_name(&graph, "ann") && has_name(&graph, "bob"));
        assert!(!has_name(&graph, "ghost"));
        // One commit for the whole transaction: one sequence number, one change set.
        assert_eq!(graph.last_commit_seq(), seq + 1);
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.events,
            vec![format!("before:{}", seq + 1), format!("after:{}", seq + 1)]
        );
        assert_eq!(seen.change_sets.len(), 1);
        let added = seen.change_sets[0]
            .mutations()
            .iter()
            .filter(|m| matches!(m, Mutation::AddVertex { .. }))
            .count();
        assert_eq!(
            added, 2,
            "the rolled-back savepoint is not in the change set"
        );
    }

    #[test]
    fn an_error_from_the_closure_rolls_back_everything() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let next = next_vertex_id(&mut graph);
        let seen = record(&mut graph);
        let before = snapshot(&graph);
        let seq = graph.last_commit_seq();
        let outcome: TraverserResult<()> = graph.transaction(|g| {
            add(g, "ann")?;
            g.traversal_mut().v("1").drop().to_list()?;
            g.traversal_mut().v("2").property("age", 99).to_list()?;
            add_then_fail(g, "ghost")?; // `?` gives up: the whole transaction is rolled back
            add(g, "never")?;
            Ok(())
        });
        assert!(outcome.is_err());
        assert_eq!(snapshot(&graph), before);
        assert_eq!(graph.last_commit_seq(), seq);
        assert_eq!(take_events(&seen), vec!["rollback:failed".to_string()]);
        // Ids handed out inside the rolled-back transaction are never reused.
        assert_ne!(next_vertex_id(&mut graph), next);
    }

    #[test]
    fn a_closure_error_of_any_type_rolls_back() {
        #[derive(Debug, PartialEq)]
        enum AppError {
            Graph,
            Business(&'static str),
        }
        impl From<GraphError> for AppError {
            fn from(_: GraphError) -> Self {
                AppError::Graph
            }
        }
        let mut graph = TraversalGraph::tinkerpop_modern();
        let before = snapshot(&graph);
        let outcome: Result<(), AppError> = graph.transaction(|g| {
            add(g, "ann").map_err(|_| AppError::Graph)?;
            Err(AppError::Business("over budget"))
        });
        assert_eq!(outcome, Err(AppError::Business("over budget")));
        assert_eq!(snapshot(&graph), before);
    }

    #[test]
    fn a_veto_rolls_back_everything_and_reaches_the_closure_error_type() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let seen = record(&mut graph);
        seen.lock().unwrap().veto = true;
        let before = snapshot(&graph);
        let seq = graph.last_commit_seq();

        // `E = BoxedTraverserError` (what `?` on a terminal produces).
        let outcome: TraverserResult<()> = graph.transaction(|g| {
            add(g, "ann")?;
            add(g, "bob")?;
            Ok(())
        });
        let err = outcome.expect_err("vetoed");
        match &*err {
            TraverserError::Graph(GraphError::CommitRejected { hook, source, .. }) => {
                assert_eq!(hook, "recorder");
                assert_eq!(source.to_string(), "refused");
            }
            other => panic!("not a veto: {other:?}"),
        }
        let help = graphersal::error::DiagnosticError::help(&err).expect("help");
        assert!(
            help.contains("query, transaction, whole script, import"),
            "{help}"
        );
        assert!(
            help.contains("Transactional::dry_run), which calls no hooks"),
            "{help}"
        );
        assert_eq!(snapshot(&graph), before);
        assert_eq!(graph.last_commit_seq(), seq);
        assert_eq!(
            take_events(&seen),
            vec![format!("before:{}", seq + 1), "rollback:vetoed".to_string()]
        );

        // `E = GraphError`.
        let outcome: Result<(), GraphError> = graph.transaction(|g| {
            g.add_vertex(None, vec!["x".into()], PropertyMap::default())?;
            Ok(())
        });
        assert!(matches!(outcome, Err(GraphError::CommitRejected { .. })));
        assert_eq!(snapshot(&graph), before);

        // `E = Box<dyn Error>`: the hook's own error is the source.
        let outcome: Result<(), Box<dyn std::error::Error>> = graph.transaction(|g| {
            add(g, "carl")?;
            Ok(())
        });
        let err = outcome.expect_err("vetoed");
        let source = std::error::Error::source(err.as_ref()).expect("the hook's error");
        assert_eq!(source.to_string(), "refused");
        assert_eq!(snapshot(&graph), before);
    }

    #[test]
    fn a_nested_transaction_is_a_savepoint() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let seen = record(&mut graph);
        let seq = graph.last_commit_seq();
        let outcome: TraverserResult<()> = graph.transaction(|g| {
            add(g, "ann")?;
            // An inner transaction that fails: only its own changes are rolled back, and the
            // hooks hear nothing (only the outermost unit commits or rolls back).
            let inner: TraverserResult<()> = g.transaction(|g| {
                add(g, "inner-failed")?;
                add_then_fail(g, "ghost")?;
                Ok(())
            });
            assert!(inner.is_err());
            // An inner transaction that succeeds keeps its changes in the outer unit.
            g.transaction(|g| add(g, "inner-kept").map(|_| ()))?;
            Ok(())
        });
        outcome.unwrap();
        assert!(has_name(&graph, "ann"));
        assert!(has_name(&graph, "inner-kept"));
        assert!(!has_name(&graph, "inner-failed"));
        assert_eq!(graph.last_commit_seq(), seq + 1);
        assert_eq!(
            take_events(&seen),
            vec![format!("before:{}", seq + 1), format!("after:{}", seq + 1)]
        );

        // The outer unit can still roll back what a committed inner transaction kept.
        let before = snapshot(&graph);
        let outcome: TraverserResult<()> = graph.transaction(|g| {
            g.transaction(|g| add(g, "inner").map(|_| ()))?;
            add_then_fail(g, "ghost")?;
            Ok(())
        });
        assert!(outcome.is_err());
        assert_eq!(snapshot(&graph), before);
    }

    #[test]
    fn transaction_metadata_reaches_the_hooks_and_nested_metadata_is_merged() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let seen = record(&mut graph);
        let outer = CommitMetadata::new()
            .with_principal("migrator")
            .with_attribute("ticket", "OPS-1");
        let outcome: TraverserResult<()> = graph.transaction_with(outer, |g| {
            add(g, "ann")?;
            let inner = CommitMetadata::new()
                .with_principal("ignored")
                .with_attribute("ticket", "ignored")
                .with_attribute("step", 2);
            g.transaction_with(inner, |g| add(g, "bob").map(|_| ()))?;
            let failed = CommitMetadata::new().with_attribute("failed", true);
            let _ = g.transaction_with(failed, |g| add_then_fail(g, "ghost").map(|_| ()));
            Ok(())
        });
        outcome.unwrap();
        // A later plain traversal commits with empty metadata again.
        add(&mut graph, "carl").unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.change_sets.len(), 2);
        let metadata = seen.change_sets[0].metadata();
        assert_eq!(metadata.principal(), Some("migrator"));
        let attributes: Vec<(String, ElementProperty)> = metadata
            .attributes()
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        assert_eq!(
            attributes,
            vec![
                ("ticket".to_string(), ElementProperty::from("OPS-1")),
                ("step".to_string(), ElementProperty::from(2)),
            ]
        );
        assert_eq!(seen.change_sets[1].metadata(), &CommitMetadata::new());
    }

    #[test]
    fn execution_mutated_inside_a_transaction_means_changed_in_the_enclosing_unit() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let outcome: TraverserResult<()> = graph.transaction(|g| {
            assert!(g.traversal_mut().add_v("x").execute().mutated());
            assert!(!g.traversal_mut().v(None::<()>).count().execute().mutated());
            assert!(!g.traversal_mut().v("nope").drop().execute().mutated());
            let failed = g
                .traversal_mut()
                .add_v("n")
                .constant("x")
                .as_number(GType::LONG)
                .execute();
            assert!(!failed.is_ok() && !failed.mutated());
            Ok(())
        });
        outcome.unwrap();
        // Outside a transaction: the outermost commit decides, as before.
        assert!(graph.traversal_mut().add_v("y").execute().mutated());
        assert!(
            !graph
                .traversal_mut()
                .v(None::<()>)
                .count()
                .execute()
                .mutated()
        );
        // Inside a dry run as well.
        let (mutated, _) = graph
            .dry_run(|g| Ok::<_, GraphError>(g.traversal_mut().add_v("z").execute().mutated()))
            .unwrap();
        assert!(mutated);
    }

    fn preview_body(g: &mut TraversalGraph) -> TraverserResult<usize> {
        add(g, "ann")?;
        g.traversal_mut().v("1").drop().to_list()?;
        g.traversal_mut().v("2").property("age", 99).to_list()?;
        // A failing traversal inside the dry run rolls back only itself.
        assert!(add_then_fail(g, "ghost").is_err());
        let (josh, peter) = (super::v(g, "4"), super::v(g, "6"));
        g.add_edge(
            None,
            Some("knows".into()),
            josh,
            peter,
            PropertyMap::default(),
        )?;
        Ok(g.vertex_count().unwrap())
    }

    #[test]
    fn a_dry_run_leaves_the_graph_untouched_and_returns_what_would_have_changed() {
        // The reference: the same body committed in a transaction on a fresh graph, as the
        // hook sees it.
        let mut committed = TraversalGraph::tinkerpop_modern();
        let reference = record(&mut committed);
        let count_in_transaction = committed.transaction(preview_body).unwrap();
        let expected = reference.lock().unwrap().change_sets[0].clone();

        let mut graph = TraversalGraph::tinkerpop_modern();
        let seen = record(&mut graph);
        let before = snapshot(&graph);
        let seq = graph.last_commit_seq();
        let (count, changes) = graph.dry_run(preview_body).unwrap();
        assert_eq!(count, count_in_transaction, "the body saw its own changes");
        assert_eq!(snapshot(&graph), before, "the graph is untouched");
        assert_eq!(graph.last_commit_seq(), seq);
        assert!(take_events(&seen).is_empty(), "no hook is called");
        assert!(seen.lock().unwrap().change_sets.is_empty());
        assert_eq!(changes.commit_seq(), 0);
        assert_eq!(changes.metadata(), &CommitMetadata::new());
        assert!(!changes.is_empty());
        assert_eq!(changes.mutations(), expected.mutations());

        // An error from the closure comes back unchanged; nothing stays.
        let failed: Result<((), ChangeSet), Box<dyn std::error::Error + Send + Sync>> = graph
            .dry_run(|g| {
                add(g, "ann").map_err(|_| "add")?;
                Err("stop".into())
            });
        assert_eq!(failed.unwrap_err().to_string(), "stop");
        assert_eq!(snapshot(&graph), before);
        assert!(take_events(&seen).is_empty());
    }

    #[test]
    fn a_dry_run_rolls_back_a_schema_change_and_a_read_only_one_is_empty() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let before = snapshot(&graph);
        let mode_before = graph.get_schema().unwrap().map(|s| s.mode);
        let (_, changes) = graph
            .dry_run(|g| {
                let mut schema = g.infer_schema().unwrap();
                schema.mode = SchemaMode::Open;
                g.set_schema(schema)
            })
            .unwrap();
        assert!(matches!(changes.mutations(), [Mutation::SetSchema { .. }]));
        assert_eq!(graph.get_schema().unwrap().map(|s| s.mode), mode_before);
        assert_eq!(snapshot(&graph), before);

        let (count, changes) = graph
            .dry_run(|g| g.traversal_mut().v(None::<()>).count().next())
            .unwrap();
        assert_eq!(count, Some(6.into()));
        assert!(changes.is_empty());
    }

    #[test]
    fn a_dry_run_inside_a_transaction_previews_only_its_own_changes() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let seen = record(&mut graph);
        let seq = graph.last_commit_seq();
        let outcome: TraverserResult<()> = graph.transaction(|g| {
            add(g, "kept")?;
            let (_, preview) = g.dry_run(|g| add(g, "previewed"))?;
            assert_eq!(preview.len(), 1);
            match &preview.mutations()[0] {
                Mutation::AddVertex { properties, .. } => {
                    assert_eq!(
                        properties.get("name"),
                        Some(&ElementProperty::from("previewed"))
                    );
                }
                other => panic!("unexpected {other:?}"),
            }
            Ok(())
        });
        outcome.unwrap();
        assert!(has_name(&graph, "kept"));
        assert!(!has_name(&graph, "previewed"));
        assert_eq!(graph.last_commit_seq(), seq + 1);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.change_sets.len(), 1);
        assert_eq!(
            seen.change_sets[0].len(),
            1,
            "only the kept vertex is committed"
        );
    }

    #[test]
    fn a_transaction_through_the_lock_of_a_shared_graph() {
        let graph: Graph = GraphSource::tinkerpop_modern();
        let outcome: TraverserResult<()> = graph.write().transaction(|g| {
            add(g, "ann")?;
            Ok(())
        });
        outcome.unwrap();
        assert_eq!(graph.read().vertex_count(), 7);
    }

    // -----------------------------------------------------------------------------------------
    // `Transactional` over every storage
    // -----------------------------------------------------------------------------------------

    /// One generic body for every storage: adds a vertex named `name` through the trait.
    fn add_named<S: GraphStorage>(storage: &mut S, id: &str) -> Result<(), GraphError> {
        storage.add_vertex(
            Some(id.into()),
            vec!["person".into()],
            super::props(&[("name", id.into())]),
        )?;
        Ok(())
    }

    /// Runs a committed transaction, a failing one and a dry run through `Transactional` on any
    /// storage and returns the dry run's change set.
    fn exercise<S: GraphStorage>(storage: &mut S) -> ChangeSet {
        storage
            .transaction(|s| add_named(s, "kept"))
            .expect("commit");
        let failed: Result<(), GraphError> = storage.transaction(|s| {
            add_named(s, "undone")?;
            Err(GraphError::unsupported("test"))
        });
        assert!(failed.is_err());
        assert!(storage.get_vertex_by_id("kept").unwrap().is_some());
        assert!(storage.get_vertex_by_id("undone").unwrap().is_none());
        let count = storage.vertex_count().unwrap();
        let ((), changes) = storage
            .dry_run(|s| add_named(s, "previewed"))
            .expect("dry run");
        assert_eq!(storage.vertex_count().unwrap(), count);
        assert!(storage.get_vertex_by_id("previewed").unwrap().is_none());
        changes
    }

    #[test]
    fn transactional_works_on_every_wrapper_with_the_same_dry_run() {
        let mut owned = TraversalGraph::tinkerpop_modern();
        let reference = exercise(&mut owned);
        assert_eq!(reference.len(), 1);
        assert_eq!(reference.commit_seq(), 0);

        let mut base = TraversalGraph::tinkerpop_modern();
        let mut by_ref = &mut base;
        assert_eq!(exercise(&mut by_ref).mutations(), reference.mutations());

        let mut boxed = Box::new(TraversalGraph::tinkerpop_modern());
        assert_eq!(exercise(&mut boxed).mutations(), reference.mutations());

        let mut arc = Arc::new(TraversalGraph::tinkerpop_modern());
        assert_eq!(exercise(&mut arc).mutations(), reference.mutations());
        assert_eq!(snapshot(&owned), snapshot(&base));
        assert_eq!(snapshot(&owned), snapshot(&boxed));
        assert_eq!(snapshot(&owned), snapshot(&arc));
    }

    #[test]
    fn a_nested_savepoint_merges_its_metadata_on_a_box() {
        let mut boxed = Box::new(TraversalGraph::new());
        let seen = record(&mut boxed);
        let outer = CommitMetadata::new()
            .with_principal("outer")
            .with_attribute("ticket", 1);
        let outcome: Result<(), GraphError> = boxed.transaction_with(outer, |s| {
            add_named(s, "a")?;
            let inner = CommitMetadata::new()
                .with_principal("inner")
                .with_attribute("ticket", 2)
                .with_attribute("step", "inner");
            s.transaction_with(inner, |s| add_named(s, "b"))?;
            let failed = CommitMetadata::new().with_attribute("failed", true);
            let refused: Result<(), GraphError> = s.transaction_with(failed, |s| {
                add_named(s, "c")?;
                Err(GraphError::unsupported("test"))
            });
            assert!(refused.is_err());
            Ok(())
        });
        outcome.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.change_sets.len(), 1);
        let metadata = seen.change_sets[0].metadata();
        assert_eq!(metadata.principal(), Some("outer"));
        let attributes: Vec<(String, ElementProperty)> = metadata
            .attributes()
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        assert_eq!(
            attributes,
            vec![
                ("ticket".to_string(), ElementProperty::from(1)),
                ("step".to_string(), ElementProperty::from("inner")),
            ]
        );
        assert_eq!(seen.change_sets[0].len(), 2, "a and b");
    }

    #[test]
    fn a_dry_run_needs_change_capture() {
        // The read-only view claims nothing: a dry run is unsupported, nothing changes.
        let graph = TraversalGraph::tinkerpop_modern();
        let mut view = &graph;
        let refused: Result<((), ChangeSet), GraphError> = view.dry_run(|_| Ok(()));
        assert!(
            matches!(refused, Err(GraphError::Unsupported { .. })),
            "{refused:?}"
        );
        // The closure's own error wins over the `Unsupported` of the discard.
        let failed: Result<((), ChangeSet), GraphError> =
            view.dry_run(|_| Err(GraphError::vertex_not_found("x")));
        assert!(matches!(failed, Err(GraphError::VertexNotFound { .. })));
        // A shared `Arc` cannot mutate and claims nothing either.
        let mut shared = Arc::new(TraversalGraph::tinkerpop_modern());
        let _other = shared.clone();
        let refused: Result<((), ChangeSet), GraphError> = shared.dry_run(|_| Ok(()));
        assert!(matches!(refused, Err(GraphError::Unsupported { .. })));
        assert_eq!(graph.vertex_count(), 6);
    }

    #[test]
    fn mark_without_a_journal_is_unavailable_through_every_wrapper() {
        fn mark_of<S: GraphStorage>(
            storage: &mut S,
        ) -> Result<graphersal::changes::Mark, GraphError> {
            storage.mark("m")
        }
        let mut graph = TraversalGraph::new();
        assert!(matches!(
            mark_of(&mut graph),
            Err(GraphError::MarkUnavailable { .. })
        ));
        let mut by_ref = &mut graph;
        assert!(matches!(
            mark_of(&mut by_ref),
            Err(GraphError::MarkUnavailable { .. })
        ));
        let mut boxed = Box::new(TraversalGraph::new());
        assert!(matches!(
            mark_of(&mut boxed),
            Err(GraphError::MarkUnavailable { .. })
        ));
        let plain = TraversalGraph::new();
        let mut view = &plain;
        assert!(matches!(
            mark_of(&mut view),
            Err(GraphError::Unsupported { .. })
        ));
    }

    #[cfg(feature = "persist")]
    #[test]
    fn mark_with_a_journal_goes_through_the_trait() {
        use graphersal::persist::{Durability, Journal, SharedBuffer};
        fn mark_of<S: GraphStorage>(
            storage: &mut S,
        ) -> Result<graphersal::changes::Mark, GraphError> {
            storage.mark("before-import")
        }
        let mut boxed = Box::new(TraversalGraph::tinkerpop_modern());
        Journal::new(SharedBuffer::new(), Durability::EveryCommit)
            .attach(&mut boxed)
            .unwrap();
        add(&mut boxed, "ann").unwrap();
        let mark = mark_of(&mut boxed).unwrap();
        assert_eq!(mark.name(), "before-import");
        assert_eq!(mark.commit_seq(), boxed.last_commit_seq().unwrap());
        // A mark names a committed state: not inside a unit.
        let inside: Result<(), GraphError> = boxed.transaction(|g| {
            assert!(matches!(
                mark_of(g),
                Err(GraphError::MarkUnavailable { .. })
            ));
            Ok(())
        });
        inside.unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// Rhai: the whole script as one unit (host option `eval_value_atomic`)
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
mod script_atomic {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::script_host::rhai::Dynamic;
    use crate::script_host::{
        ScriptGraphTraversalSource, ScriptLimits, eval_value, eval_value_atomic, render_value,
    };
    use graphersal::exec::RenderOptions;
    use graphersal::prelude::*;

    use super::explicit::record;

    fn atomic(graph: &Arc<Graph>, script: &str) -> Result<Dynamic, String> {
        eval_value_atomic(
            graph.clone(),
            script,
            HashMap::new(),
            &ScriptLimits::default(),
        )
        .map_err(|err| err.to_string())
    }

    fn count(graph: &Arc<Graph>, label: &str) -> i64 {
        eval_value(
            graph.clone(),
            &format!(r#"g.V().hasLabel("{label}").count().next()"#),
        )
        .unwrap()
        .as_int()
        .unwrap()
    }

    const FAILS_LATER: &str = r#"
        g.addV("a").next();
        g.V("1").property("age", 99).next();
        g.V().values("name").asNumber(GType.LONG).toList();
    "#;

    #[test]
    fn a_failing_later_statement_rolls_back_the_earlier_ones() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let seen = record(&mut *graph.write());
        let seq = graph.read().last_commit_seq();
        assert!(atomic(&graph, FAILS_LATER).is_err());
        assert_eq!(count(&graph, "a"), 0);
        let age = eval_value(graph.clone(), r#"g.V("1").values("age").next()"#).unwrap();
        assert_eq!(age.as_int().unwrap(), 29);
        assert_eq!(graph.read().last_commit_seq(), seq);
        assert_eq!(
            seen.lock().unwrap().events,
            vec!["rollback:failed".to_string()]
        );
    }

    #[test]
    fn by_default_the_earlier_statements_stay() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        assert!(eval_value(graph.clone(), FAILS_LATER).is_err());
        assert_eq!(count(&graph, "a"), 1);
    }

    #[test]
    fn a_successful_script_commits_once() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let seen = record(&mut *graph.write());
        let seq = graph.read().last_commit_seq();
        let value = atomic(
            &graph,
            r#"
            g.addV("a").next();
            g.addV("a").next();
            // A failure the script handles rolls back only its own statement.
            let r = g.addV("a").constant("x").asNumber(GType.LONG).execute();
            let w = g.addV("b").execute();
            [r.error == (), r.mutated, w.mutated]
            "#,
        )
        .unwrap()
        .into_array()
        .unwrap();
        let flags: Vec<bool> = value.into_iter().map(|v| v.as_bool().unwrap()).collect();
        assert_eq!(
            flags,
            vec![false, false, true],
            "r failed, w changed the unit"
        );
        assert_eq!(count(&graph, "a"), 2);
        assert_eq!(count(&graph, "b"), 1);
        assert_eq!(graph.read().last_commit_seq(), seq + 1);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.change_sets.len(), 1);
        assert_eq!(seen.change_sets[0].len(), 3);
    }

    #[test]
    fn a_veto_rolls_back_the_whole_script_and_is_an_error() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let seen = record(&mut *graph.write());
        seen.lock().unwrap().veto = true;
        let err = atomic(&graph, r#"g.addV("a").next(); g.addV("a").next();"#).unwrap_err();
        assert!(
            err.contains("rejected by the commit hook 'recorder'"),
            "{err}"
        );
        assert_eq!(graph.read().vertex_count(), 6);
    }

    #[test]
    fn a_returned_traversal_runs_against_the_real_graph_after_the_commit() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let value = atomic(&graph, r#"g.addV("a").next(); g.V().hasLabel("a").count()"#).unwrap();
        let text = render_value(value, VisualizeFormat::Auto, &RenderOptions::unlimited()).text;
        assert!(text.contains("│ 1 "), "{text}");
        let _ = atomic(&graph, r#"g.addV("a").next();"#).unwrap();
        // Also inside a collection.
        let value = atomic(&graph, r#"[1, #{t: g.V().hasLabel("a").count()}]"#).unwrap();
        let mut items = value.into_array().unwrap();
        let mut map = items
            .remove(1)
            .try_cast::<crate::script_host::rhai::Map>()
            .unwrap();
        let mut traversal = map
            .remove("t")
            .unwrap()
            .try_cast::<ScriptGraphTraversalSource>()
            .unwrap();
        // Against the private graph of the finished unit it would count nothing.
        assert_eq!(traversal.to_list().unwrap().len(), 1);
        assert_eq!(traversal.next().unwrap().as_int().unwrap(), 2);
    }

    #[test]
    fn parameters_limits_and_the_graph_after_a_script_error() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let params = HashMap::from([("label".to_string(), ElementProperty::from("p"))]);
        let value = eval_value_atomic(
            graph.clone(),
            r#"g.addV(label).next(); g.V().hasLabel(label).count().next()"#,
            params,
            &ScriptLimits::default(),
        )
        .unwrap();
        assert_eq!(value.as_int().unwrap(), 1);
        // A resource limit aborts the script: everything is rolled back, the graph is usable.
        let limits = ScriptLimits::default().with_max_operations(10_000);
        let err = eval_value_atomic(
            graph.clone(),
            r#"g.addV("q").next(); loop { }"#,
            HashMap::new(),
            &limits,
        );
        assert!(err.is_err());
        assert_eq!(count(&graph, "q"), 0);
        assert_eq!(count(&graph, "p"), 1);
    }
}

// ---------------------------------------------------------------------------------------------
// Rhai: a dry run of the whole script (host option `eval_value_dry_run`)
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
mod script_dry_run {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::script_host::rhai::{Dynamic, Map};
    use crate::script_host::{
        ScriptLimits, eval_value, eval_value_dry_run, materialize_traversals,
    };
    use graphersal::changes::ChangeSet;
    use graphersal::prelude::*;

    use super::explicit::record;

    fn dry_run(graph: &Arc<Graph>, script: &str) -> Result<(Dynamic, ChangeSet), String> {
        eval_value_dry_run(
            graph.clone(),
            script,
            HashMap::new(),
            &ScriptLimits::default(),
        )
        .map_err(|err| err.to_string())
    }

    #[test]
    fn reports_the_changes_and_leaves_the_graph_untouched() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let seen = record(&mut *graph.write());
        let seq = graph.read().last_commit_seq();
        let (value, changes) = dry_run(
            &graph,
            r#"
            g.addV("person").property("name", "ann").next();
            g.V("1").property("age", 99).next();
            // A failure the script handles rolls back only its own statement.
            let r = g.addV("x").constant("x").asNumber(GType.LONG).execute();
            g.V().count().next()
            "#,
        )
        .unwrap();
        assert_eq!(value.as_int().unwrap(), 7);
        assert_eq!(changes.len(), 2, "the handled failure left no mutation");
        assert_eq!(changes.commit_seq(), 0);
        // The graph, the commit sequence and the hooks are untouched.
        assert_eq!(graph.read().vertex_count(), 6);
        let age = eval_value(graph.clone(), r#"g.V("1").values("age").next()"#).unwrap();
        assert_eq!(age.as_int().unwrap(), 29);
        assert_eq!(graph.read().last_commit_seq(), seq);
        let seen = seen.lock().unwrap();
        assert!(seen.change_sets.is_empty());
        assert!(seen.events.is_empty(), "{:?}", seen.events);
    }

    #[test]
    fn a_vetoing_hook_is_not_consulted() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let seen = record(&mut *graph.write());
        seen.lock().unwrap().veto = true;
        let (_, changes) = dry_run(&graph, r#"g.addV("a").next();"#).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(graph.read().vertex_count(), 6);
    }

    #[test]
    fn a_failing_script_is_an_error_and_rolls_back() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let err = dry_run(
            &graph,
            r#"g.addV("a").next(); g.V().values("name").asNumber(GType.LONG).toList();"#,
        )
        .unwrap_err();
        assert!(
            err.contains("as_number") || err.contains("asNumber"),
            "{err}"
        );
        assert_eq!(graph.read().vertex_count(), 6);
        // The graph is usable afterwards.
        let value = eval_value(graph.clone(), r#"g.addV("b").next(); g.V().count().next()"#);
        assert_eq!(value.unwrap().as_int().unwrap(), 7);
    }

    #[test]
    fn a_returned_closure_is_rejected() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let err = dry_run(&graph, r#"g.addV("a").next(); [1, #{f: |x| x + 1}]"#).unwrap_err();
        assert!(
            err.contains("cannot return a closure or function pointer (result[1].f)"),
            "{err}"
        );
        assert_eq!(graph.read().vertex_count(), 6);
    }

    #[test]
    fn a_returned_traversal_runs_inside_the_dry_run() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let (value, changes) = dry_run(
            &graph,
            r#"g.addV("a").next(); [g.V().hasLabel("a").count(), #{t: g.V().hasLabel("a").count()}, g]"#,
        )
        .unwrap();
        assert_eq!(changes.len(), 1);
        let mut items = value.into_array().unwrap();
        // It saw the previewed vertex, though the graph never kept it.
        let first = items[0].clone().into_array().unwrap();
        assert_eq!(first[0].as_int().unwrap(), 1);
        let mut map = items.remove(1).try_cast::<Map>().unwrap();
        let nested = map.remove("t").unwrap().into_array().unwrap();
        assert_eq!(nested[0].as_int().unwrap(), 1);
        // A bare source yields an empty list.
        assert!(items[1].clone().into_array().unwrap().is_empty());
        assert_eq!(graph.read().vertex_count(), 6);
    }

    #[test]
    fn parameters_and_a_resource_limit() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let params = HashMap::from([("label".to_string(), ElementProperty::from("p"))]);
        let (value, _) = eval_value_dry_run(
            graph.clone(),
            r#"g.addV(label).next(); g.V().hasLabel(label).count().next()"#,
            params,
            &ScriptLimits::default(),
        )
        .unwrap();
        assert_eq!(value.as_int().unwrap(), 1);
        let limits = ScriptLimits::default().with_max_operations(10_000);
        let err = eval_value_dry_run(
            graph.clone(),
            r#"g.addV("q").next(); loop { }"#,
            HashMap::new(),
            &limits,
        );
        assert!(err.is_err());
        assert_eq!(graph.read().vertex_count(), 6);
    }

    #[test]
    fn materialize_traversals_runs_returned_traversals_as_to_list() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let value = eval_value(
            graph,
            r#"#{names: g.V().hasLabel("software").values("name").order(), n: 1}"#,
        )
        .unwrap();
        let mut map = materialize_traversals(value, 0)
            .unwrap()
            .try_cast::<Map>()
            .unwrap();
        let names: Vec<String> = map
            .remove("names")
            .unwrap()
            .into_array()
            .unwrap()
            .into_iter()
            .map(|v| v.into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["lop".to_string(), "ripple".to_string()]);
        assert_eq!(map.remove("n").unwrap().as_int().unwrap(), 1);
    }
}

/// `iterate()` runs a traversal for its side effects as one committed unit and returns nothing.
#[test]
fn iterate_commits_the_side_effects_of_a_traversal() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let seq = graph.last_commit_seq();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "ann")
        .iterate()
        .unwrap();
    assert_eq!(graph.last_commit_seq(), seq + 1);
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .has("name", "ann")
            .count()
            .next()
            .unwrap(),
        Some(1.into())
    );
    // A read-only traversal iterates too (and changes nothing).
    graph
        .traversal()
        .v(None)
        .out(None)
        .out(None)
        .iterate()
        .unwrap();
    assert_eq!(graph.last_commit_seq(), seq + 1);
}

// ---------------------------------------------------------------------------------------------
// A panic in a commit hook: `before_commit` rolls the unit back, `after_commit` keeps it
// ---------------------------------------------------------------------------------------------

mod hook_panics {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use graphersal::changes::{ChangeSet, CommitHook, HookError, RollbackReason};
    use graphersal::prelude::*;

    use super::snapshot;

    /// Panics in `before_commit` while `before` is set and in `after_commit` while `after` is
    /// set; records the rollbacks it is told about.
    #[derive(Clone, Default)]
    struct Panicky {
        before: Arc<AtomicBool>,
        after: Arc<AtomicBool>,
        rollbacks: Arc<Mutex<Vec<String>>>,
    }

    impl CommitHook for Panicky {
        fn name(&self) -> &str {
            "panicky"
        }

        fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
            if self.before.load(Ordering::SeqCst) {
                panic!("before_commit panics");
            }
            Ok(())
        }

        fn after_commit(&mut self, _: &ChangeSet) {
            if self.after.load(Ordering::SeqCst) {
                panic!("after_commit panics");
            }
        }

        fn after_rollback(&mut self, reason: &RollbackReason) {
            let text = match reason {
                RollbackReason::Failed { message, .. } => message.clone(),
                other => format!("{other:?}"),
            };
            self.rollbacks.lock().unwrap().push(text);
        }
    }

    fn panicky(graph: &mut TraversalGraph) -> Panicky {
        let hook = Panicky::default();
        graph.register_commit_hook(Box::new(hook.clone())).unwrap();
        hook.before.store(true, Ordering::SeqCst);
        hook
    }

    /// Runs `unit` with the hook panicking in `before_commit`: the panic reaches the caller,
    /// the graph and its commit position are unchanged, the hook was told about the rollback,
    /// and the next unit (the hook calm again) commits normally.
    fn rolls_back(what: &str, unit: impl FnOnce(&mut TraversalGraph)) {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let hook = panicky(&mut graph);
        let before = snapshot(&graph);
        let seq = graph.last_commit_seq();
        let outcome = catch_unwind(AssertUnwindSafe(|| unit(&mut graph)));
        assert!(
            outcome.is_err(),
            "{what}: the panic continues to the caller"
        );
        assert_eq!(snapshot(&graph), before, "{what}: rolled back");
        assert_eq!(graph.last_commit_seq(), seq, "{what}: no commit");
        let rollbacks = hook.rollbacks.lock().unwrap().clone();
        assert_eq!(rollbacks.len(), 1, "{what}: {rollbacks:?}");
        assert!(rollbacks[0].contains("panic"), "{what}: {rollbacks:?}");
        hook.before.store(false, Ordering::SeqCst);
        graph
            .traversal_mut()
            .add_v("person")
            .property("name", "next")
            .to_list()
            .unwrap();
        assert_eq!(
            graph.last_commit_seq(),
            seq + 1,
            "{what}: the next unit commits"
        );
        assert_eq!(graph.vertex_count(), 7, "{what}");
    }

    #[test]
    fn a_panic_in_before_commit_rolls_back_every_unit_opener() {
        rolls_back("to_list", |g| {
            let _ = g.traversal_mut().add_v("person").to_list();
        });
        rolls_back("next", |g| {
            let _ = g.traversal_mut().v("1").property("age", 99).next();
        });
        rolls_back("iterate", |g| {
            let _ = g.traversal_mut().v(None).drop().iterate();
        });
        rolls_back("execute", |g| {
            let _ = g.traversal_mut().add_v("person").execute();
        });
        rolls_back("profile", |g| {
            let _ = g.traversal_mut().add_v("person").profile();
        });
        rolls_back("transaction", |g| {
            let _ = g.transaction(|g| {
                g.traversal_mut().add_v("a").to_list()?;
                g.traversal_mut().v("1").drop().to_list()?;
                Ok::<_, Box<TraverserError>>(())
            });
        });
        rolls_back("set_schema", |g| {
            let schema = g.traversal().infer_schema().unwrap();
            let _ = g.traversal_mut().set_schema(schema);
        });
        rolls_back("apply_change_set", |g| {
            let (_, changes) = g
                .dry_run(|g| g.traversal_mut().add_v("person").to_list())
                .unwrap();
            let _ = g.apply_change_set(&changes);
        });
    }

    #[cfg(feature = "io")]
    #[test]
    fn a_panic_in_before_commit_rolls_back_an_import() {
        use graphersal::io::GraphImport;
        rolls_back("import_graphson", |g| {
            let line = r#"{"id":"x1","label":"person","properties":{}}"#;
            let _ = g.import_graphson_reader(line.as_bytes());
        });
        rolls_back("import_graphml", |g| {
            let xml = r#"<?xml version="1.0"?><graphml xmlns="http://graphml.graphdrawing.org/xmlns"><graph edgedefault="directed"><node id="x1"/></graph></graphml>"#;
            let _ = g.import_graphml_reader(xml.as_bytes());
        });
    }

    #[test]
    fn a_panic_in_after_commit_keeps_the_unit_committed() {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let hook = panicky(&mut graph);
        hook.before.store(false, Ordering::SeqCst);
        hook.after.store(true, Ordering::SeqCst);
        let seq = graph.last_commit_seq();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _ = graph.traversal_mut().add_v("person").to_list();
        }));
        assert!(outcome.is_err(), "the panic continues to the caller");
        assert_eq!(graph.vertex_count(), 7, "the unit stays committed");
        assert_eq!(graph.last_commit_seq(), seq + 1);
        assert!(hook.rollbacks.lock().unwrap().is_empty());
        // The unit was closed: the next one commits on its own.
        hook.after.store(false, Ordering::SeqCst);
        graph.traversal_mut().add_v("person").to_list().unwrap();
        assert_eq!(graph.last_commit_seq(), seq + 2);
        assert_eq!(graph.vertex_count(), 8);
    }

    #[test]
    fn a_panic_in_after_rollback_does_not_replace_the_first_one() {
        struct Both;
        impl CommitHook for Both {
            fn before_commit(&mut self, _: &ChangeSet) -> Result<(), HookError> {
                panic!("first");
            }
            fn after_rollback(&mut self, _: &RollbackReason) {
                panic!("second");
            }
        }
        let mut graph = TraversalGraph::tinkerpop_modern();
        graph.register_commit_hook(Box::new(Both)).unwrap();
        let payload = catch_unwind(AssertUnwindSafe(|| {
            let _ = graph.traversal_mut().add_v("person").to_list();
        }))
        .unwrap_err();
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"first"));
        assert_eq!(graph.vertex_count(), 6);
        assert_eq!(graph.last_commit_seq(), 0);
    }

    #[cfg(feature = "script")]
    #[test]
    fn a_panic_in_before_commit_rolls_back_a_script_unit() {
        use crate::script_host::{eval_value, eval_value_atomic};

        let mut storage = TraversalGraph::tinkerpop_modern();
        let hook = panicky(&mut storage);
        let graph = Arc::new(Graph::new(storage));
        for (what, atomic, script) in [
            ("per traversal", false, r#"g.addV("person").next(); 1"#),
            (
                "whole script",
                true,
                r#"g.addV("person").next(); g.V("1").drop().iterate(); 1"#,
            ),
        ] {
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                if atomic {
                    let _ = eval_value_atomic(
                        graph.clone(),
                        script,
                        Default::default(),
                        &Default::default(),
                    );
                } else {
                    let _ = eval_value(graph.clone(), script);
                }
            }));
            assert!(outcome.is_err(), "{what}");
            assert_eq!(graph.read().vertex_count(), 6, "{what}");
            assert_eq!(graph.read().edge_count(), 6, "{what}");
            assert_eq!(graph.read().last_commit_seq(), 0, "{what}");
        }
        hook.before.store(false, Ordering::SeqCst);
        let _ = eval_value_atomic(
            graph.clone(),
            r#"g.addV("person").next(); 1"#,
            Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(graph.read().vertex_count(), 7);
        assert_eq!(graph.read().last_commit_seq(), 1);
    }
}
