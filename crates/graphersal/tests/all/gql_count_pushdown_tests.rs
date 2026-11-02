//! GQL whole-pattern counts (issue #20, feature `gql`): `count(x)` of a variable that is never
//! `NULL` counts like `count(*)` (no path label for `x`), an aggregation that only counts is
//! `count()` + the row, and `MATCH (n:L) RETURN count(n)` / `MATCH ()-[r:T]->() RETURN count(r)`
//! are answered from the statistics like `g.v().count()` / `g.e().count()`. Every count is
//! compared with the walk (the rows of the same pattern) on a graph with removed elements,
//! self-loops, multi-labels and either-direction edges; the plans are checked through the
//! profile; a count over many edges runs under a small `traversal.max_materialized_bytes`.
#![cfg(feature = "gql")]

use graphersal::prelude::*;

/// A graph with multi-labels, self-loops, parallel edges, unlabelled nodes and removed
/// elements (a vertex with its edges, and one edge on its own).
fn fixture() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    for query in [
        "INSERT (a:A {i: 1}), (b:B {i: 2}), (c:A:B {i: 3}), (d {i: 4}), (x:A {i: 9}), \
         (a)-[:T]->(b), (b)-[:T]->(c), (c)-[:U]->(a), (a)-[:T]->(a), (d)-[:U]->(d), \
         (c)-[:T]->(d), (a)-[:U]->(b), (a)-[:T]->(b), (x)-[:T]->(a), (b)-[:U]->(x), \
         (a)-[:V]->(c)",
        "MATCH (x {i: 9}) DETACH DELETE x",
        "MATCH ()-[r:V]->() DELETE r",
    ] {
        graph
            .traversal_mut()
            .gql(query)
            .iterate()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
    }
    graph
}

fn rows(graph: &TraversalGraph, query: &str) -> Vec<ElementProperty> {
    graph
        .traversal()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

/// The one integer column `key` of the one row of `query`.
fn count(graph: &TraversalGraph, query: &str, key: &str) -> i64 {
    let rows = rows(graph, query);
    assert_eq!(rows.len(), 1, "{query}: {rows:?}");
    integer(&rows[0], key, query)
}

/// The integer column `key` of `row`.
fn integer(row: &ElementProperty, key: &str, query: &str) -> i64 {
    let value = match row {
        ElementProperty::Map(map) => map.get_str(key).cloned(),
        ElementProperty::Object(map) => map.get(key).cloned(),
        other => panic!("{query}: not a row map: {other:?}"),
    };
    match value {
        Some(ElementProperty::Number(n)) if !n.is_f64() => n.as_i64().expect("an integer"),
        other => panic!("{query}: column {key} is {other:?}"),
    }
}

/// The number of rows `MATCH <pattern> RETURN 1` walks (the reference count).
fn walked(graph: &TraversalGraph, pattern: &str) -> i64 {
    rows(graph, &format!("MATCH {pattern} RETURN 1 AS one")).len() as i64
}

/// The steps of the profile of `query`, without the metrics columns.
fn steps(graph: &TraversalGraph, query: &str) -> Vec<String> {
    let profile = graph
        .traversal()
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .to_string();
    profile
        .lines()
        .skip_while(|line| !line.starts_with("====="))
        .skip(1)
        .take_while(|line| !line.starts_with("====="))
        .filter(|line| {
            let line = line.trim_start();
            !line.starts_with('[') && !line.starts_with("\\>") && !line.contains("TOTAL:")
        })
        .map(|line| {
            let step = line.trim_start();
            step[..step.find("  ").unwrap_or(step.len())].to_string()
        })
        .collect()
}

/// The patterns whose count every form must give exactly as the walk.
const PATTERNS: &[(&str, &str)] = &[
    ("(n)", "n"),
    ("(n:A)", "n"),
    ("(n:B)", "n"),
    ("(n:A|B)", "n"),
    ("(n:A&B)", "n"),
    ("(n:A:B)", "n"),
    ("(n:Missing)", "n"),
    ("(n {i: 1})", "n"),
    ("(n:A WHERE n.i > 1)", "n"),
    ("()-[r]->()", "r"),
    ("()<-[r]-()", "r"),
    ("()-[r:T]->()", "r"),
    ("()<-[r:U]-()", "r"),
    ("()-[r:T|U]->()", "r"),
    ("()-[r:V]->()", "r"),
    ("()-[r:Missing]->()", "r"),
    ("()-[r]-()", "r"),
    ("()-[r:T]-()", "r"),
    ("()-[r:U]-()", "r"),
    ("(a:A)-[r]->()", "r"),
    ("()-[r]->(b:B)", "r"),
    ("(a)-[r:T]->(b) WHERE a.i < b.i", "r"),
    ("(a)-[r]->(a)", "r"),
    ("(a)-[r {k: 1}]->()", "r"),
    ("(a)-[r]->(b)-[s]->(c)", "s"),
    ("(a)-[r]->(b)", "a"),
    ("(a)-[r]->(b)", "b"),
];

#[test]
fn counts_equal_the_walk_for_every_form() {
    let graph = fixture();
    for (pattern, variable) in PATTERNS {
        let expected = walked(&graph, pattern);
        for (ret, key) in [
            (format!("count({variable}) AS c"), "c"),
            ("count(*) AS c".to_string(), "c"),
            (format!("count({variable})"), &*format!("count({variable})")),
            ("count(*)".to_string(), "count(*)"),
        ] {
            let query = format!("MATCH {pattern} RETURN {ret}");
            assert_eq!(count(&graph, &query, key), expected, "{query}");
        }
        // Several counts in one row, and the same count through a WITH.
        let query = format!("MATCH {pattern} RETURN count({variable}) AS a, count(*) AS b");
        assert_eq!(count(&graph, &query, "a"), expected, "{query}");
        assert_eq!(count(&graph, &query, "b"), expected, "{query}");
        let query = format!("MATCH {pattern} WITH count({variable}) AS c RETURN c");
        assert_eq!(count(&graph, &query, "c"), expected, "{query}");
        // A count the rewrite leaves alone (not the whole column) agrees too.
        let query = format!("MATCH {pattern} RETURN count({variable}) + 0 AS c");
        assert_eq!(count(&graph, &query, "c"), expected, "{query}");
    }
}

#[test]
fn the_fixture_counts_are_the_hand_computed_ones() {
    let graph = fixture();
    // Live: a, b, c, d; edges T a->b, T b->c, U c->a, T a->a, U d->d, T c->d, U a->b, T a->b.
    for (query, expected) in [
        ("MATCH (n) RETURN count(n) AS c", 4),
        ("MATCH (n:A) RETURN count(n) AS c", 2),
        ("MATCH (n:A&B) RETURN count(n) AS c", 1),
        ("MATCH ()-[r]->() RETURN count(r) AS c", 8),
        ("MATCH ()-[r:T]->() RETURN count(r) AS c", 5),
        ("MATCH ()-[r:V]->() RETURN count(r) AS c", 0),
        // Either direction: every edge twice, a self-loop (T a->a, U d->d) once.
        ("MATCH ()-[r]-() RETURN count(r) AS c", 14),
        ("MATCH ()-[r:T]-() RETURN count(r) AS c", 9),
    ] {
        assert_eq!(count(&graph, query, "c"), expected, "{query}");
    }
    let empty = TraversalGraph::new();
    for query in [
        "MATCH (n) RETURN count(n) AS c",
        "MATCH ()-[r]->() RETURN count(r) AS c",
        "MATCH ()-[r]-() RETURN count(*) AS c",
    ] {
        assert_eq!(count(&empty, query, "c"), 0, "{query}");
    }
}

#[test]
fn distinct_counts_keep_their_aggregate() {
    let graph = fixture();
    for (query, expected) in [
        ("MATCH (n) RETURN count(DISTINCT n) AS c", 4),
        ("MATCH ()-[r]-() RETURN count(DISTINCT r) AS c", 8),
        ("MATCH (a)-[r]->(b) RETURN count(DISTINCT a) AS c", 4),
    ] {
        assert_eq!(count(&graph, query, "c"), expected, "{query}");
        assert!(
            steps(&graph, query)
                .iter()
                .any(|s| s.starts_with("gql:aggregate(")),
            "{query}: {:?}",
            steps(&graph, query)
        );
    }
}

#[test]
fn a_variable_that_may_be_null_is_still_counted_as_written() {
    let graph = fixture();
    // `m` of an OPTIONAL MATCH is NULL where nothing matches: count(m) <> count(*).
    let query = "MATCH (n) OPTIONAL MATCH (n)-[:U]->(m) RETURN count(m) AS cm, count(*) AS c";
    assert_eq!(count(&graph, query, "cm"), 3);
    assert_eq!(count(&graph, query, "c"), 4);
    // A conditional singleton (`?`) is NULL after zero iterations.
    let query = "MATCH (a) ((x)-[e:U]->(y))? (b) RETURN count(x) AS cx, count(*) AS c";
    assert_eq!(count(&graph, query, "cx"), 3);
    assert_eq!(count(&graph, query, "c"), 7);
    // A WITH column may hold NULL.
    let query = "MATCH (n) WITH CASE WHEN n.i > 2 THEN n END AS m RETURN count(m) AS c";
    assert_eq!(count(&graph, query, "c"), 2);
    // A FOR item may be NULL.
    let query = "FOR x IN [1, NULL, 2] RETURN count(x) AS c";
    assert_eq!(count(&graph, query, "c"), 2);
}

#[test]
fn grouped_counts_equal_the_walk() {
    let graph = fixture();
    let grouped = |query: &str| -> Vec<String> {
        let mut out: Vec<String> = rows(&graph, query)
            .iter()
            .map(|row| format!("{row:?}"))
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        grouped("MATCH (a)-[r]->() RETURN a.i AS k, count(r) AS c"),
        grouped("MATCH (a)-[r]->() RETURN a.i AS k, count(DISTINCT r) AS c"),
    );
    let total: i64 = rows(&graph, "MATCH (a)-[r]-() RETURN a.i AS k, count(r) AS c")
        .iter()
        .map(|row| integer(row, "c", "grouped"))
        .sum();
    assert_eq!(total, walked(&graph, "(a)-[r]-()"));
}

#[test]
fn whole_pattern_counts_are_answered_from_the_statistics() {
    let graph = fixture();
    for (query, plan) in [
        (
            "MATCH (n) RETURN count(n) AS c",
            vec!["v().count()", "gql:project(\"c\")"],
        ),
        (
            "MATCH (n) RETURN count(*)",
            vec!["v().count()", "gql:project(\"count(*)\")"],
        ),
        (
            "MATCH (n:A) RETURN count(n) AS c",
            vec!["v(labels: [\"A\"]).count()", "gql:project(\"c\")"],
        ),
        (
            "MATCH (n:A|B) RETURN count(*) AS c",
            vec!["v(labels: [\"A\", \"B\"]).count()", "gql:project(\"c\")"],
        ),
        (
            "MATCH ()-[r]->() RETURN count(r) AS c",
            vec!["e().count()", "gql:project(\"c\")"],
        ),
        (
            "MATCH ()<-[r]-() RETURN count(*) AS c",
            vec!["e().count()", "gql:project(\"c\")"],
        ),
        (
            "MATCH ()-[r:T]->() RETURN count(r) AS c",
            vec!["e(labels: [\"T\"]).count()", "gql:project(\"c\")"],
        ),
        (
            "MATCH ()-[r:T|U]->() RETURN count(r) AS c, count(*) AS d",
            vec![
                "e(labels: [\"T\", \"U\"]).count()",
                "gql:project(\"c\", \"d\")",
            ],
        ),
    ] {
        assert_eq!(steps(&graph, query), plan, "{query}");
    }
}

#[test]
fn other_counts_walk_without_labelling_the_counted_variable() {
    let graph = fixture();
    for (query, plan) in [
        // Either direction: a self-loop counts once, so no statistic answers it.
        (
            "MATCH ()-[r]-() RETURN count(r) AS c",
            vec![
                "v()",
                "union(__.out(), __.in_e().gql:not_self_loop().out_v())",
                "gql:aggregate([], [\"count(r)\"], [\"c\"])",
            ],
        ),
        // A pushed property filter on the scan counts after it.
        (
            "MATCH (n:A) WHERE n.i > 1 RETURN count(n) AS c",
            vec![
                "v(labels: [\"A\"])",
                "has(\"i\", P.gt(1))",
                "count()",
                "gql:project(\"c\")",
            ],
        ),
        // A labelled endpoint walks the edges.
        (
            "MATCH (a:A)-[r]->() RETURN count(r) AS c",
            vec![
                "v(labels: [\"A\"])",
                "out()",
                "gql:aggregate([], [\"count(r)\"], [\"c\"])",
            ],
        ),
    ] {
        assert_eq!(steps(&graph, query), plan, "{query}");
    }
    // A filter over two nodes labels only the first, never the counted edge.
    let plan = steps(
        &graph,
        "MATCH (a)-[r]->(b) WHERE a.i < b.i RETURN count(r) AS c",
    );
    assert!(plan.iter().all(|s| !s.contains("as(\"r\")")), "{plan:?}");
    assert!(plan.iter().any(|s| s.starts_with("as(\"a\")")), "{plan:?}");
    // A count in a writing query keeps its aggregation.
    let mut graph = fixture();
    let plan = graph
        .traversal_mut()
        .gql("MATCH (n:B) DETACH DELETE n RETURN count(*) AS c")
        .profile()
        .expect("profile")
        .to_string();
    assert!(plan.contains("gql:aggregate("), "{plan}");
}

#[test]
fn a_pushed_count_runs_under_a_small_materialization_limit() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .gql("UNWIND range(0, 99) AS i INSERT (:P {i: i})")
        .iterate()
        .expect("vertices");
    graph
        .traversal_mut()
        .gql("MATCH (a:P), (b:P) WHERE a.i < 60 INSERT (a)-[:E]->(b)")
        .iterate()
        .expect("edges");
    let limited = |query: &str| {
        graph
            .traversal()
            .with("traversal.max_materialized_bytes", 50_000i64)
            .gql(query)
            .to_list()
    };
    // 6000 edges walked are far beyond 50 000 bytes ...
    let walk = limited("MATCH ()-[r]->() RETURN count(r) + 0 AS c");
    match walk.as_ref().map_err(|e| e.root_cause()) {
        Err(TraverserError::ResourceLimitExceeded { limit, .. }) => {
            assert_eq!(limit, "traversal.max_materialized_bytes")
        }
        other => panic!("expected the limit, got {other:?}"),
    }
    // ... the statistics are not.
    for query in [
        "MATCH ()-[r]->() RETURN count(r) AS c",
        "MATCH ()-[r:E]->() RETURN count(*) AS c",
        "MATCH (n:P) RETURN count(n) AS c",
    ] {
        let got = limited(query).unwrap_or_else(|e| panic!("{query}: {e}"));
        let expected = if query.contains("(n:P)") { 100 } else { 6000 };
        assert_eq!(
            format!("{got:?}"),
            format!("{:?}", rows(&graph, query)),
            "{query}"
        );
        assert_eq!(count(&graph, query, "c"), expected, "{query}");
    }
}
