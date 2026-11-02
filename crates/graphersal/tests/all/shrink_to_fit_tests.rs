//! `g.shrink_to_fit()` / `Graph::shrink_to_fit` (issue #22, book page "Memory Usage and
//! Profiling"): a graph built by write queries keeps the spare capacity of its slot vectors
//! and indexes; the shrink releases it, reports `memory_usage()`'s total before and after,
//! changes no query result, keeps the vacant slots of removed elements (handles stay valid),
//! commits nothing (no journal record; a Store reopens to the same data), and a second call
//! releases nothing.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::script::rhai::{Dynamic, Map};
use graphersal::storage::IntoAnyGraph;

use crate::script_host::*;

fn eval(graph: &Arc<Graph>, script: &str) -> Dynamic {
    eval_value(graph.clone(), script).unwrap_or_else(|err| panic!("{script}: {err}"))
}

/// Runs `script` for its effect.
fn run(graph: &Arc<Graph>, script: &str) {
    let _ = eval(graph, script);
}

fn int(graph: &Arc<Graph>, script: &str) -> i64 {
    eval(graph, script)
        .as_int()
        .unwrap_or_else(|_| panic!("{script}: not an int"))
}

fn map(value: Dynamic) -> Map {
    value.try_cast::<Map>().expect("a map")
}

fn field(map: &Map, key: &str) -> i64 {
    map[key]
        .as_int()
        .unwrap_or_else(|_| panic!("{key}: not an int in {map:?}"))
}

/// `(vertex spare, edge spare, vertex tombstoned, edge tombstoned)` of the memory report.
fn slots(graph: &Arc<Graph>) -> (i64, i64, i64, i64) {
    let structure = map(eval(graph, "g.memory_report().structure"));
    let vertex = map(structure["vertex_slots"].clone());
    let edge = map(structure["edge_slots"].clone());
    (
        field(&vertex, "spare"),
        field(&edge, "spare"),
        field(&vertex, "tombstoned"),
        field(&edge, "tombstoned"),
    )
}

fn total_bytes(graph: &Arc<Graph>) -> i64 {
    int(graph, "g.memory_usage().total_bytes")
}

/// A graph built by queries only: 3000 vertices, 6000 edges (vector doubling leaves spare
/// capacity: 3000 is not a power of two).
fn built_by_queries() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::empty());
    run(
        &graph,
        r#"g.gql("UNWIND range(0, 2999) AS i CREATE (:P {id: i, name: 'p' + toString(i)})").iterate()"#,
    );
    run(&graph, r#"g.create_index(#{label: "P", key: "id"})"#);
    run(
        &graph,
        r#"g.gql("UNWIND range(0, 5999) AS k MATCH (a:P {id: k % 3000}), (b:P {id: (k * 7 + 13) % 3000}) CREATE (a)-[:E {k: k}]->(b)").iterate()"#,
    );
    graph
}

/// Query results that must not change: counts, a property sum, an adjacency fingerprint.
fn answers(graph: &Arc<Graph>) -> Vec<String> {
    [
        "g.v().count().next()",
        "g.e().count().next()",
        r#"g.v().values("id").sum().next()"#,
        r#"g.e().values("k").sum().next()"#,
        r#"g.v().has("P", "id", 17).out("E").values("id").order().to_list()"#,
        r#"g.v().has("P", "id", 42).in_("E").values("name").order().to_list()"#,
        r#"g.v().has("P", "id", P.lt(5)).values("name").order().to_list()"#,
    ]
    .iter()
    .map(|query| format!("{:?}", eval(graph, query)))
    .collect()
}

#[test]
fn a_graph_built_by_queries_releases_its_spare_slots() {
    let graph = built_by_queries();
    let (vertex_spare, edge_spare, _, _) = slots(&graph);
    assert!(
        vertex_spare > 0 && edge_spare > 0,
        "{vertex_spare} {edge_spare}"
    );
    let before = total_bytes(&graph);
    let expected = answers(&graph);
    let seq = graph.read().last_commit_seq();

    let report = map(eval(&graph, "g.shrink_to_fit()"));
    assert_eq!(report["measured"].as_bool(), Ok(true));
    let released = field(&report, "released_bytes");
    assert!(released > 0, "{report:?}");
    assert_eq!(field(&report, "before_bytes"), before);
    let after = total_bytes(&graph);
    assert_eq!(field(&report, "after_bytes"), after);
    assert_eq!(released, before - after);

    let (vertex_spare, edge_spare, _, _) = slots(&graph);
    assert_eq!((vertex_spare, edge_spare), (0, 0));
    assert_eq!(answers(&graph), expected);
    // Nothing committed.
    assert_eq!(graph.read().last_commit_seq(), seq);

    // A second call releases nothing.
    let again = map(eval(&graph, "g.shrink_to_fit()"));
    assert_eq!(field(&again, "released_bytes"), 0);
    assert_eq!(field(&again, "before_bytes"), after);

    // The graph keeps working: a write after the shrink grows it again.
    run(&graph, r#"g.add_v("P").property("id", 3000).iterate()"#);
    assert_eq!(int(&graph, "g.v().count().next()"), 3001);
}

#[test]
fn vacant_slots_of_removed_elements_stay_and_handles_stay_valid() {
    let graph = built_by_queries();
    run(
        &graph,
        r#"g.v().has("P", "id", P.lt(100)).drop().iterate()"#,
    );
    let (_, _, vertex_tombs, edge_tombs) = slots(&graph);
    assert_eq!(vertex_tombs, 100);
    assert!(edge_tombs > 0);
    let expected = answers(&graph);
    // A handle held across the shrink.
    let (kept, kept_id) = {
        let g = graph.read();
        let kept = g.vertices().unwrap().next().expect("a vertex");
        (kept, g.vertex_id(kept).unwrap().map(str::to_string))
    };

    let report = graph.shrink_to_fit().unwrap();
    assert!(report.released_bytes().unwrap() > 0);
    let (vertex_spare, edge_spare, vertex_after, edge_after) = slots(&graph);
    assert_eq!((vertex_spare, edge_spare), (0, 0));
    assert_eq!((vertex_after, edge_after), (vertex_tombs, edge_tombs));
    assert_eq!(answers(&graph), expected);
    {
        let g = graph.read();
        assert!(g.contains_vertex(kept).unwrap());
        assert_eq!(g.vertex_id(kept).unwrap().map(str::to_string), kept_id);
    }
    // A new vertex reuses a vacant slot: no spare capacity is needed.
    run(&graph, r#"g.add_v("P").property("id", -1).iterate()"#);
    let (_, _, vertex_tombs_now, _) = slots(&graph);
    assert_eq!(vertex_tombs_now, vertex_tombs - 1);
}

#[test]
fn the_rust_api_and_the_dyn_boundary_report_the_same_figures() {
    let graph = built_by_queries();
    let any = graph.clone().into_any_graph();
    let before = any.memory_usage().unwrap().unwrap().total_bytes();
    let report = any.shrink_to_fit().unwrap();
    assert_eq!(report.before_bytes, Some(before));
    assert_eq!(
        report.after_bytes,
        Some(any.memory_usage().unwrap().unwrap().total_bytes())
    );
    assert!(report.released_bytes().unwrap() > 0);
    // The footprint figure (cached by commit position) follows the shrink.
    assert_eq!(
        any.memory_footprint_bytes().unwrap(),
        report.after_bytes,
        "the cached footprint must not keep the pre-shrink figure"
    );
    assert_eq!(graph.shrink_to_fit().unwrap().released_bytes(), Some(0));
}

#[test]
fn the_cached_footprint_is_walked_again_after_a_direct_shrink() {
    let graph = built_by_queries();
    let before = graph.read().memory_footprint_bytes().unwrap();
    GraphStorage::shrink_to_fit(&mut *graph.write()).unwrap();
    let after = graph.read().memory_footprint_bytes().unwrap();
    assert!(after < before, "{after:?} < {before:?}");
    assert_eq!(
        after,
        graph
            .read()
            .memory_usage()
            .unwrap()
            .map(|stats| stats.total_bytes())
    );
}

#[test]
fn inside_one_atomic_script_the_shrink_keeps_the_unit_intact() {
    let graph = Arc::new(GraphSource::empty());
    // Writes, the shrink and a failure in one unit: the rollback restores the empty graph.
    let failed = eval_value_atomic(
        graph.clone(),
        r#"g.gql("UNWIND range(0, 999) AS i CREATE (:P {id: i})").iterate();
           g.shrink_to_fit();
           g.v().has("P", "id", 5).drop().iterate();
           throw "fail on purpose";"#,
        Default::default(),
        &ScriptLimits::default(),
    );
    assert!(failed.is_err());
    assert_eq!(graph.read().vertex_count(), 0);
    // And a committed one keeps every write made around it.
    let _ = eval_value_atomic(
        graph.clone(),
        r#"g.gql("UNWIND range(0, 999) AS i CREATE (:P {id: i})").iterate();
           g.shrink_to_fit();
           g.v().has("P", "id", 5).drop().iterate();"#,
        Default::default(),
        &ScriptLimits::default(),
    )
    .unwrap();
    assert_eq!(graph.read().vertex_count(), 999);
}

#[cfg(all(feature = "persist", not(target_arch = "wasm32")))]
mod store {
    use super::*;
    use graphersal::persist::{Store, StoreOptions};

    use crate::persist_tests::dump;

    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn journal_bytes(dir: &std::path::Path) -> u64 {
        let wal = dir.join("wal");
        std::fs::read_dir(&wal)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok()?.metadata().ok())
                    .map(|meta| meta.len())
                    .sum()
            })
            .unwrap_or(0)
    }

    #[test]
    fn under_a_store_the_shrink_writes_no_commit_and_recovery_matches() {
        let dir = TempDir(
            std::env::temp_dir().join(format!("graphersal-shrink-store-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir.0);
        let store = Store::create(&dir.0, StoreOptions::new()).unwrap();
        let graph = store.graph().clone();
        let _ = eval_value(
            graph.clone(),
            r#"g.gql("UNWIND range(0, 2999) AS i CREATE (:P {id: i})").iterate();
               g.create_index(#{label: "P", key: "id"});
               g.gql("UNWIND range(0, 2999) AS i MATCH (a:P {id: i}), (b:P {id: (i + 1) % 3000}) CREATE (a)-[:E]->(b)").iterate();
               g.v().has("P", "id", P.lt(10)).drop().iterate();"#,
        )
        .unwrap();
        let seq = graph.read().last_commit_seq();
        let wal = journal_bytes(&dir.0);
        assert!(wal > 0, "the writes are in the journal");

        let report = map(eval(&graph, "g.shrink_to_fit()"));
        assert!(field(&report, "released_bytes") > 0, "{report:?}");
        assert_eq!(graph.read().last_commit_seq(), seq, "no commit");
        assert_eq!(journal_bytes(&dir.0), wal, "no journal record");

        let expected = dump(&graph.read());
        drop(graph);
        drop(store);
        let store = Store::open(&dir.0, StoreOptions::new()).unwrap();
        assert_eq!(store.graph().read().last_commit_seq(), seq);
        assert_eq!(dump(&store.graph().read()), expected);
    }
}
