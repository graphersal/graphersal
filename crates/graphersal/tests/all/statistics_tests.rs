//! `GraphStatistics`: the counts `TraversalGraph` keeps incrementally must always equal a full
//! recomputation (`GraphStatistics::from_storage`): after every mutation kind, after a rollback,
//! after a replay and after a GraphML load. Also the Rust and Rhai read API.

use std::sync::Arc;

use std::sync::Mutex;

use graphersal::changes::ChangeSet;
use graphersal::prelude::*;
use graphersal::storage::GraphStatistics;
use graphersal::{
    changes::{CommitHook, HookError, RollbackReason},
    storage::VertexHandle,
};

/// A commit hook that keeps the change sets it accepts.
struct Recorder(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0
            .lock()
            .map_err(|_| HookError::from("poisoned"))?
            .push(changes.clone());
        Ok(())
    }
}

/// Asserts the kept statistics equal a full recomputation and returns a copy.
fn exact(graph: &TraversalGraph) -> GraphStatistics {
    let kept = graph
        .statistics()
        .unwrap()
        .expect("TraversalGraph keeps statistics");
    assert_eq!(kept, &GraphStatistics::from_storage(graph));
    kept.clone()
}

fn v(graph: &TraversalGraph, id: &str) -> VertexHandle {
    graph.get_vertex_by_id(id).expect("vertex")
}

#[test]
fn modern_graph_statistics() {
    let graph = TraversalGraph::tinkerpop_modern();
    let stats = exact(&graph);
    assert_eq!((stats.vertex_count(), stats.edge_count()), (6, 6));
    assert_eq!(
        stats.vertex_label_counts(),
        vec![("person", 4), ("software", 2)]
    );
    assert_eq!(
        stats.primary_vertex_label_counts(),
        vec![("person", 4), ("software", 2)]
    );
    assert_eq!(
        stats.edge_label_counts(),
        vec![("created", 4), ("knows", 2)]
    );
    assert_eq!(stats.vertex_label_count("nope"), 0);
    // The traversal source passes the same statistics through.
    assert_eq!(graph.traversal().statistics().unwrap(), Some(&stats));
}

#[test]
fn every_mutation_kind_keeps_the_statistics_exact() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let a = graph
        .add_vertex(
            Some("a".into()),
            vec!["admin".into(), "person".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let stats = exact(&graph);
    assert_eq!(stats.vertex_label_count("person"), 5);
    assert_eq!(stats.primary_vertex_label_count("person"), 4);
    assert_eq!(stats.primary_vertex_label_count("admin"), 1);

    let unlabeled = graph
        .add_vertex(None, vec![], PropertyMap::default())
        .unwrap();
    assert_eq!(exact(&graph).vertex_count(), 8);

    let e = graph
        .add_edge(
            None,
            Some("manages".into()),
            a,
            v(&graph, "1"),
            PropertyMap::default(),
        )
        .unwrap();
    graph
        .add_edge(None, None, a, unlabeled, PropertyMap::default())
        .unwrap();
    assert_eq!(exact(&graph).edge_label_count("manages"), 1);

    // Adding a label that is not first leaves the primary label alone.
    graph
        .add_vertex_label(v(&graph, "1"), "admin".into())
        .unwrap();
    assert_eq!(exact(&graph).primary_vertex_label_count("admin"), 1);
    // Removing the primary label promotes the next one.
    graph.remove_vertex_label(a, "admin").unwrap();
    let stats = exact(&graph);
    assert_eq!(stats.primary_vertex_label_count("admin"), 0);
    assert_eq!(stats.primary_vertex_label_count("person"), 5);
    assert_eq!(stats.vertex_label_count("admin"), 1);
    // A label on an unlabeled vertex becomes its primary one.
    graph.add_vertex_label(unlabeled, "thing".into()).unwrap();
    exact(&graph);

    // Property writes do not change counts.
    graph
        .set_vertex_property(unlabeled, "name".into(), "x".into())
        .unwrap();
    exact(&graph);

    graph.drop_edge(e).unwrap();
    assert_eq!(exact(&graph).edge_label_count("manages"), 0);
    // Dropping a vertex takes its incident edges with it.
    graph.drop_vertex(v(&graph, "1")).unwrap();
    let stats = exact(&graph);
    assert_eq!(stats.edge_label_count("knows"), 0);
    assert_eq!(stats.vertex_label_count("admin"), 0);
    graph.drop_vertex(a).unwrap();
    graph.drop_vertex(unlabeled).unwrap();
    assert_eq!(exact(&graph).vertex_count(), 5);
}

#[test]
fn rollback_restores_the_statistics() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = exact(&graph);
    let mark = graph.begin_unit().unwrap();
    let n = graph
        .add_vertex(
            None,
            vec!["x".into(), "person".into()],
            PropertyMap::default(),
        )
        .unwrap();
    graph
        .add_edge(
            None,
            Some("y".into()),
            n,
            v(&graph, "2"),
            PropertyMap::default(),
        )
        .unwrap();
    graph.add_vertex_label(v(&graph, "3"), "x".into()).unwrap();
    graph.remove_vertex_label(v(&graph, "4"), "person").unwrap();
    graph.drop_vertex(v(&graph, "1")).unwrap();
    assert_ne!(exact(&graph), before);
    graph
        .rollback_unit(mark, &RollbackReason::failed(&"test"))
        .unwrap();
    assert_eq!(exact(&graph), before);
}

#[test]
fn a_replay_rebuilds_the_statistics() {
    let mut source = TraversalGraph::tinkerpop_modern();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    source
        .register_commit_hook(Box::new(Recorder(recorded.clone())))
        .expect("TraversalGraph supports hooks");
    let mark = source.begin_unit().unwrap();
    let n = source
        .add_vertex(
            Some("n".into()),
            vec!["a".into(), "b".into()],
            PropertyMap::default(),
        )
        .unwrap();
    source
        .add_edge(
            None,
            Some("l".into()),
            n,
            v(&source, "1"),
            PropertyMap::default(),
        )
        .unwrap();
    source.remove_vertex_label(n, "a").unwrap();
    source.drop_vertex(v(&source, "2")).unwrap();
    source.commit_unit(mark).unwrap();
    let set = recorded.lock().unwrap().remove(0);
    let replica = TraversalGraph::tinkerpop_modern().replay(&set).unwrap();
    assert_eq!(exact(&replica), exact(&source));
}

#[cfg(feature = "io")]
#[test]
fn a_graphml_load_rebuilds_the_statistics() {
    use graphersal::io::{GraphExport, GraphImport};

    let graph = GraphSource::tinkerpop_modern();
    graph
        .write()
        .add_vertex(
            None,
            vec!["admin".into(), "person".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let expected = exact(&graph.read());
    let path = std::env::temp_dir().join(format!(
        "graphersal_statistics_{}.graphml",
        std::process::id()
    ));
    let path = path.to_str().unwrap().to_string();
    graph.export_graphml(&path).expect("export");
    let mut loaded = GraphSource::empty();
    loaded.import_graphml(&path).expect("import");
    let _ = std::fs::remove_file(&path);
    assert_eq!(exact(&loaded.read()), expected);
}

#[cfg(feature = "script")]
#[test]
fn statistics_in_the_script_dsl() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let map = crate::script_host::eval_value(
        graph,
        r#"g.addV(["admin", "person"]).to_list(); g.statistics()"#,
    )
    .unwrap()
    .cast::<rhai::Map>();
    assert_eq!(map["vertex_count"].as_int().unwrap(), 7);
    assert_eq!(map["edge_count"].as_int().unwrap(), 6);
    let labels = map["vertex_labels"].clone().cast::<rhai::Map>();
    assert_eq!(labels["person"].as_int().unwrap(), 5);
    assert_eq!(labels["admin"].as_int().unwrap(), 1);
    let primary = map["primary_vertex_labels"].clone().cast::<rhai::Map>();
    assert_eq!(primary["person"].as_int().unwrap(), 4);
    assert_eq!(primary["admin"].as_int().unwrap(), 1);
    let edges = map["edge_labels"].clone().cast::<rhai::Map>();
    assert_eq!(edges["created"].as_int().unwrap(), 4);
}
