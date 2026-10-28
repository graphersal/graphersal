//! `dedup()` inside a `repeat()` body keeps one seen-set for the whole loop (TinkerPop), while a
//! `dedup()` in a per-traverser child of the body (`where`, `local`) starts fresh for every
//! traverser. Both orders agree and merging the frontier stays invisible.

#[cfg(feature = "script")]
use std::sync::Arc;

use graphersal::prelude::*;

type Source<'b> = GraphTraversalSource<'b, &'b TraversalGraph>;

/// A graph of vertices named after their ids, with `child` edges (edges inserted in reverse so
/// `out()` follows the listed order).
fn graph_of(vertices: &[&str], edges: &[(&str, &str)]) -> Graph {
    let mut graph = TraversalGraph::new();
    let mut ids = std::collections::HashMap::new();
    for id in vertices {
        let mut props = graphersal::prelude::PropertyMap::default();
        props.insert("name", ElementProperty::String((*id).into()));
        ids.insert(
            *id,
            graph
                .add_vertex(Some((*id).into()), vec!["node".into()], props)
                .unwrap(),
        );
    }
    for (from, to) in edges.iter().rev() {
        graph
            .add_edge(
                None,
                Some("child".into()),
                ids[from],
                ids[to],
                Default::default(),
            )
            .unwrap();
    }
    Graph::new(graph)
}

/// s -> a, s -> b, a -> b, b -> c
fn small() -> Graph {
    graph_of(
        &["s", "a", "b", "c"],
        &[("s", "a"), ("s", "b"), ("a", "b"), ("b", "c")],
    )
}

fn rust_values<F>(graph: &Graph, order: &str, merge: bool, query: F) -> Vec<ElementProperty>
where
    F: for<'b> Fn(Source<'b>) -> Source<'b>,
{
    let lock = graph.read();
    query(
        lock.traversal()
            .with("repeat.order", order)
            .with("bulk.merge", merge),
    )
    .to_list()
    .expect("query runs")
}

fn sorted_text(values: &[ElementProperty]) -> Vec<String> {
    let mut text: Vec<String> = values
        .iter()
        .map(|value| match value {
            ElementProperty::Vertex(v) => v
                .properties()
                .get("name")
                .and_then(|n| n.as_string())
                .unwrap_or("?")
                .to_string(),
            ElementProperty::String(s) => s.to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    text.sort();
    text
}

#[cfg(feature = "script")]
fn count_of(values: &[ElementProperty]) -> i64 {
    values.first().and_then(|v| v.as_i64()).expect("a count")
}

/// Evaluates a Rhai traversal and returns the materialized results.
#[cfg(feature = "script")]
fn rhai_on(graph: Arc<Graph>, script: &str) -> Vec<ElementProperty> {
    let engine = crate::script_host::engine();
    let mut scope = crate::script_host::graph_scope(graph.clone());
    let gts = engine
        .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
        .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
    let lock = graph.read();
    gts.ast_convert_to_gts(&*lock)
        .to_list()
        .expect("script runs")
}

#[cfg(feature = "script")]
#[test]
fn the_three_tinkerpop_scenarios() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    for (script, expected) in [
        ("g.V().emit().repeat(__.dedup()).times(2).count()", 12),
        ("g.V().repeat(__.dedup()).times(2).count()", 0),
        ("g.V().repeat(__.dedup().both()).times(2).count()", 0),
        ("g.V().emit().repeat(__.dedup()).times(2).count()", 12),
    ] {
        assert_eq!(
            count_of(&rhai_on(graph.clone(), script)),
            expected,
            "{script}"
        );
    }
    let lock = graph.read();
    let g = || lock.traversal();
    let rust = g()
        .v(None)
        .emit()
        .repeat(__::dedup())
        .times(2)
        .count()
        .to_list()
        .unwrap();
    assert_eq!(count_of(&rust), 12);
    let rust = g()
        .v(None)
        .repeat(__::dedup())
        .times(2)
        .count()
        .to_list()
        .unwrap();
    assert_eq!(count_of(&rust), 0);
    let rust = g()
        .v(None)
        .repeat(__::dedup().both(None))
        .times(2)
        .count()
        .to_list()
        .unwrap();
    assert_eq!(count_of(&rust), 0);
}

#[cfg(feature = "script")]
#[test]
fn a_visited_set_over_the_whole_loop() {
    let graph = small();
    for order in ["bfs", "dfs"] {
        for merge in [false, true] {
            let res = rust_values(&graph, order, merge, |g| {
                g.v("s").repeat(__::out(None).dedup()).emit()
            });
            assert_eq!(
                sorted_text(&res),
                vec!["a", "b", "c"],
                "{order} merge={merge}"
            );
        }
    }
    let res = rhai_on(
        Arc::new(small()),
        r#"g.V("s").repeat(__.out().dedup()).emit()"#,
    );
    assert_eq!(sorted_text(&res), vec!["a", "b", "c"]);
}

#[cfg(feature = "script")]
#[test]
fn per_traverser_children_start_fresh() {
    // a -> t, b -> t: `where` and `local` children see one traverser at a time.
    let graph = graph_of(&["a", "b", "t"], &[("a", "t"), ("b", "t")]);
    let res = rust_values(&graph, "bfs", true, |g| {
        g.v("t")
            .in_(None)
            .repeat(AnonymousTraversal::new().where_t(__::out(None).dedup()))
            .times(1)
    });
    assert_eq!(sorted_text(&res), vec!["a", "b"]);
    let res = rust_values(&graph, "bfs", true, |g| {
        g.v("t")
            .in_(None)
            .repeat(__::local(__::out(None).dedup()))
            .times(1)
    });
    assert_eq!(sorted_text(&res), vec!["t", "t"]);
    let res = rhai_on(
        Arc::new(graph_of(&["a", "b", "t"], &[("a", "t"), ("b", "t")])),
        r#"g.V("a", "b").repeat(__.local(__.out().dedup())).times(1)"#,
    );
    assert_eq!(sorted_text(&res), vec!["t", "t"]);
}

#[test]
fn separate_dedup_steps_have_separate_sets() {
    let graph = small();
    // Iteration 1: dedup#1 sees s, out -> a, b, dedup#2 sees a, b. Iteration 2: dedup#1 sees a, b
    // (new to it), out -> b, c, dedup#2 drops b (seen), keeps c. One shared set would drop a, b
    // at dedup#1 and return nothing.
    let res = rust_values(&graph, "bfs", true, |g| {
        g.v("s").repeat(__::dedup().out(None).dedup()).times(2)
    });
    assert_eq!(sorted_text(&res), vec!["c"]);
}

#[cfg(feature = "script")]
#[test]
fn dedup_by_a_projection_in_a_body() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let res = rhai_on(
        graph.clone(),
        r#"g.V().repeat(__.out().dedup().by(T.label)).emit().count()"#,
    );
    // One survivor per label over the whole loop: a person and a software.
    assert_eq!(count_of(&res), 2);
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .repeat(__::out(None).dedup().by(By::Label))
        .emit()
        .count()
        .to_list()
        .unwrap();
    assert_eq!(count_of(&res), 2);
}

#[test]
fn a_nested_repeat_has_its_own_frame() {
    // a -> a. Every run of the inner loop starts with an empty seen-set.
    let graph = graph_of(&["a"], &[("a", "a")]);
    let res = rust_values(&graph, "bfs", true, |g| {
        g.v("a")
            .repeat(__::repeat(__::out(None).dedup()).times(1))
            .times(2)
    });
    assert_eq!(sorted_text(&res), vec!["a"]);
}

#[test]
fn bulk_merge_and_orders_agree_on_the_modern_graph() {
    let graph = GraphSource::tinkerpop_modern();
    let queries: [fn(Source<'_>) -> Source<'_>; 2] = [
        |g| g.v(None).repeat(__::both(None).dedup()).emit(),
        |g| g.v(None).repeat(__::both(None).dedup()).times(3),
    ];
    for query in queries {
        let mut results = Vec::new();
        for order in ["bfs", "dfs"] {
            for merge in [false, true] {
                results.push(sorted_text(&rust_values(&graph, order, merge, query)));
            }
        }
        assert!(
            results.windows(2).all(|pair| pair[0] == pair[1]),
            "{results:?}"
        );
    }
}

/// TinkerPop 3.8.2 runs a `limit()` in a repeat body once per iteration (feature files:
/// `repeat(both().limit(1)).times(2)` has a count of 1, `repeat(union(constant('y').limit(1),
/// identity())).times(2)` yields `y` twice), so the engine's per-iteration `limit()` is the same
/// semantic and must not turn loop-wide like `dedup()`.
#[test]
fn limit_in_the_body_counts_per_iteration() {
    let graph = small();
    for merge in [false, true] {
        let values = rust_values(&graph, "bfs", merge, |g| {
            g.v(None).repeat(__::out(None).limit(1)).times(2)
        });
        // A loop-wide counter would let the second iteration through nothing.
        assert_eq!(sorted_text(&values), vec!["b".to_string()], "merge {merge}");
    }
}
