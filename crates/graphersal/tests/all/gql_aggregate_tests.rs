//! GQL stage 1b aggregation (feature `gql`): `count(*)`, `count`, `sum`, `avg`, `min`, `max`,
//! `collect_list` (Cypher `collect`), `DISTINCT` inside an aggregate, implicit grouping by the
//! non-aggregated `RETURN` / `WITH` items (by element and by value, GQL equality), empty input
//! with and without grouping keys, bulk (a merged run equals an unmerged one), the `large`
//! sample, and aggregation after a `WITH` and followed by a `MATCH`. Rows are compared as
//! multisets unless the query sorts (design D18): against a Gremlin twin
//! (`group().by(..).by(..)`, `count()`, `fold()`) where the NULL semantics agree, else against
//! hand-computed rows.
#![cfg(feature = "gql")]

use std::sync::Arc;

use graphersal::prelude::*;

fn rows(graph: &TraversalGraph, query: &str) -> Vec<ElementProperty> {
    graph
        .traversal()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

fn profile(graph: &TraversalGraph, query: &str) -> String {
    graph
        .traversal()
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .to_string()
}

/// The steps of a profile, without the metrics columns.
fn steps(profile: &str) -> Vec<String> {
    profile
        .lines()
        .skip_while(|line| !line.starts_with("====="))
        .skip(1)
        .take_while(|line| !line.starts_with("====="))
        .filter(|line| !line.trim_start().starts_with('[') && !line.contains("TOTAL:"))
        .map(|line| {
            let step = line.trim_start();
            step[..step.find("  ").unwrap_or(step.len())].to_string()
        })
        .collect()
}

/// A value as short text: `null`, a string as is, an integer or a float as Rust prints it, a
/// list as `[a, b]` (sorted when `sort_lists`: `collect_list` keeps arrival order, D18).
fn text(value: &ElementProperty, sort_lists: bool) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => s.to_string(),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => match n.as_i64() {
            Some(i) if !n.is_f64() => i.to_string(),
            _ => format!("{:?}", n.as_f64().unwrap_or_default()),
        },
        ElementProperty::Array(items) => {
            let mut items: Vec<String> = items.iter().map(|v| text(v, sort_lists)).collect();
            if sort_lists {
                items.sort();
            }
            format!("[{}]", items.join(", "))
        }
        other => format!("{other:?}"),
    }
}

/// The column `key` of a row map.
fn column(row: &ElementProperty, key: &str) -> ElementProperty {
    match row {
        ElementProperty::Map(map) => map.get_str(key).cloned(),
        ElementProperty::Object(map) => map.get(key).cloned(),
        other => panic!("not a row map: {other:?}"),
    }
    .unwrap_or_else(|| panic!("no column {key} in {row:?}"))
}

/// Every row as `key=value` pairs in the given key order; the rows sorted unless `ordered`.
fn table(rows: &[ElementProperty], keys: &[&str], ordered: bool) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|row| {
            keys.iter()
                .map(|key| format!("{key}={}", text(&column(row, key), true)))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    if !ordered {
        out.sort();
    }
    out
}

fn check(graph: &TraversalGraph, query: &str, keys: &[&str], expected: &[&str]) {
    let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(table(&rows(graph, query), keys, false), expected, "{query}");
}

fn check_ordered(graph: &TraversalGraph, query: &str, keys: &[&str], expected: &[&str]) {
    assert_eq!(table(&rows(graph, query), keys, true), expected, "{query}");
}

/// The modern graph plus persons with a stored `null` age, a float age equal to josh's, a
/// string age and no age (the NULL fixture of `gql_tests`).
fn null_fixture() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let mut add = |name: &str, age: Option<ElementProperty>| {
        let t = graph
            .traversal_mut()
            .add_v(vec!["person"])
            .property("name", name);
        let t = match age {
            Some(age) => t.property("age", age),
            None => t,
        };
        t.iterate().unwrap();
    };
    add("nullage", Some(ElementProperty::Null));
    add("floatage", Some(ElementProperty::from(32.0)));
    add("strage", Some(ElementProperty::from("x")));
    add("noage", None);
    graph
}

// ----- every aggregate -----

#[test]
fn every_aggregate_on_the_modern_graph() {
    let graph = TraversalGraph::tinkerpop_modern();
    check(
        &graph,
        "MATCH (p:person) RETURN count(*) AS n, count(p.age) AS c, sum(p.age) AS s, \
         avg(p.age) AS a, min(p.age) AS mi, max(p.age) AS ma, collect(p.name) AS names, \
         collect_list(p.age) AS ages",
        &["n", "c", "s", "a", "mi", "ma", "names", "ages"],
        &[
            "n=4 c=4 s=123 a=30.75 mi=27 ma=35 names=[josh, marko, peter, vadas] \
           ages=[27, 29, 32, 35]",
        ],
    );
    // sum of floats is a float, avg is always a float, min / max of strings.
    check(
        &graph,
        "MATCH ()-[e]->() RETURN sum(e.weight) AS s, avg(e.weight) AS a, min(type(e)) AS mi, \
         max(type(e)) AS ma",
        &["s", "a", "mi", "ma"],
        &["s=3.5 a=0.5833333333333334 mi=created ma=knows"],
    );
    // The default column name is the item's text.
    let result = rows(&graph, "MATCH (p:person) RETURN count(*)");
    assert_eq!(table(&result, &["count(*)"], false), ["count(*)=4"]);
    // An aggregate inside an expression, and a key that is an expression.
    check(
        &graph,
        "MATCH (p:person)-[:created]->(s) RETURN p.age > 30 AS old, count(s) + 1 AS c, \
         10 * max(p.age) AS m",
        &["old", "c", "m"],
        &["old=false c=2 m=290", "old=true c=4 m=350"],
    );
}

#[test]
fn aggregates_skip_null_and_count_star_counts_rows() {
    let graph = null_fixture();
    // `strage` (a string age) is kept out of the numeric aggregates.
    check(
        &graph,
        "MATCH (p:person) WHERE p.name <> 'strage' RETURN count(*) AS n, count(p.age) AS c, \
         sum(p.age) AS s, avg(p.age) AS a, min(p.age) AS mi, max(p.age) AS ma, \
         collect(p.age) AS l",
        &["n", "c", "s", "a", "mi", "ma", "l"],
        // 29 + 27 + 32 + 35 + 32.0: a float among the inputs makes the sum a float.
        &["n=7 c=5 s=155.0 a=31.0 mi=27 ma=35 l=[27, 29, 32, 32.0, 35]"],
    );
    // min / max order across kinds (the ORDER BY order: strings before numbers), never an error.
    check(
        &graph,
        "MATCH (p:person) RETURN min(p.age) AS mi, max(p.age) AS ma",
        &["mi", "ma"],
        &["mi=x ma=35"],
    );
    // All values NULL: count 0, sum 0, the others NULL / [].
    check(
        &graph,
        "MATCH (p:person) WHERE p.age IS NULL RETURN count(*) AS n, count(p.age) AS c, \
         sum(p.age) AS s, avg(p.age) AS a, min(p.age) AS mi, collect(p.age) AS l",
        &["n", "c", "s", "a", "mi", "l"],
        &["n=2 c=0 s=0 a=null mi=null l=[]"],
    );
}

/// The GQLSTATUS and message of a failed GQL query.
fn gql_error(graph: &TraversalGraph, query: &str) -> (String, String) {
    let error = match graph.traversal().gql(query).to_list() {
        Ok(rows) => panic!("{query}: expected an error, got {rows:?}"),
        Err(error) => error,
    };
    match error.root_cause() {
        TraverserError::Gql {
            gqlstatus, message, ..
        } => (gqlstatus.clone(), message.clone()),
        other => panic!("{query}: not a GQL error: {other}"),
    }
}

#[test]
fn aggregate_data_errors_name_the_function() {
    let graph = null_fixture();
    for (query, function) in [
        ("MATCH (p:person) RETURN sum(p.age) AS s", "sum()"),
        ("MATCH (p:person) RETURN avg(p.age) AS s", "avg()"),
    ] {
        let (status, message) = gql_error(&graph, query);
        assert_eq!(status, "22G03", "{query}");
        assert!(message.contains(function), "{message}");
        assert!(message.contains("not a string"), "{message}");
    }
    let mut graph = TraversalGraph::tinkerpop_modern();
    for _ in 0..2 {
        graph
            .traversal_mut()
            .add_v(vec!["big"])
            .property("n", i64::MAX)
            .iterate()
            .unwrap();
    }
    let (status, _) = gql_error(&graph, "MATCH (b:big) RETURN sum(b.n) AS s");
    assert_eq!(status, "22003");
}

// ----- DISTINCT -----

#[test]
fn distinct_aggregates_use_gql_equality() {
    let graph = null_fixture();
    // 32 and 32.0 are one value (decision C3), NULL is skipped, the bulk is ignored.
    check(
        &graph,
        "MATCH (p:person) WHERE p.name <> 'strage' RETURN count(DISTINCT p.age) AS c, \
         sum(DISTINCT p.age) AS s, avg(DISTINCT p.age) AS a, collect(DISTINCT p.age) AS l",
        &["c", "s", "a", "l"],
        &["c=4 s=123 a=30.75 l=[27, 29, 32, 35]"],
    );
    let modern = TraversalGraph::tinkerpop_modern();
    check(
        &modern,
        "MATCH (p:person)-[:created]->(s) RETURN count(s) AS c, count(DISTINCT s) AS d, \
         collect(DISTINCT s.name) AS names",
        &["c", "d", "names"],
        &["c=4 d=2 names=[lop, ripple]"],
    );
}

// ----- grouping -----

#[test]
fn grouping_by_value_uses_gql_equality() {
    let graph = null_fixture();
    // josh's 32 and floatage's 32.0 group together (first arrival kept as the key value);
    // the stored NULL and the missing age group together.
    check(
        &graph,
        "MATCH (p:person) RETURN p.age AS a, count(*) AS c, count(p.age) AS ca, \
         collect(p.name) AS ns",
        &["a", "c", "ca", "ns"],
        &[
            "a=27 c=1 ca=1 ns=[vadas]",
            "a=29 c=1 ca=1 ns=[marko]",
            "a=32 c=2 ca=2 ns=[floatage, josh]",
            "a=35 c=1 ca=1 ns=[peter]",
            "a=null c=2 ca=0 ns=[noage, nullage]",
            "a=x c=1 ca=1 ns=[strage]",
        ],
    );
    // Several keys.
    let modern = TraversalGraph::tinkerpop_modern();
    check(
        &modern,
        "MATCH (a)-[e]->(b) RETURN type(e) AS t, b.lang AS l, count(*) AS c",
        &["t", "l", "c"],
        &["t=created l=java c=4", "t=knows l=null c=2"],
    );
}

#[test]
fn grouping_by_element_returns_the_element() {
    let graph = TraversalGraph::tinkerpop_modern();
    let result = rows(
        &graph,
        "MATCH (p:person)-[:created]->(s) RETURN s, count(p) AS c, collect(p.name) AS ps",
    );
    let mut seen: Vec<String> = result
        .iter()
        .map(|row| {
            let s = column(row, "s");
            assert!(
                matches!(s, ElementProperty::Vertex(_)),
                "the key is the vertex: {s:?}"
            );
            format!(
                "{} {}",
                text(&column(row, "c"), true),
                text(&column(row, "ps"), true)
            )
        })
        .collect();
    seen.sort();
    assert_eq!(seen, ["1 [josh]", "3 [josh, marko, peter]"]);
}

// ----- empty input -----

#[test]
fn empty_input_without_keys_gives_one_row_with_keys_none() {
    let graph = TraversalGraph::tinkerpop_modern();
    check(
        &graph,
        "MATCH (p:nobody) RETURN count(*) AS n, count(p) AS c, sum(p.age) AS s, avg(p.age) AS a, \
         min(p.age) AS mi, max(p.age) AS ma, collect(p) AS l, count(DISTINCT p) AS d",
        &["n", "c", "s", "a", "mi", "ma", "l", "d"],
        &["n=0 c=0 s=0 a=null mi=null ma=null l=[] d=0"],
    );
    assert!(rows(&graph, "MATCH (p:nobody) RETURN p.name AS k, count(*) AS c").is_empty());
    assert!(rows(&graph, "MATCH (p:nobody) RETURN p, count(*) AS c").is_empty());
    // The same through an aggregating WITH.
    check(
        &graph,
        "MATCH (p:nobody) WITH count(*) AS c RETURN c + 1 AS d",
        &["d"],
        &["d=1"],
    );
    assert!(
        rows(
            &graph,
            "MATCH (p:nobody) WITH p.name AS n, count(*) AS c RETURN n, c"
        )
        .is_empty()
    );
    // A WHERE that removes every row before the aggregation.
    check(
        &graph,
        "MATCH (p:person) WHERE p.age > 100 RETURN count(*) AS n, collect(p.name) AS l",
        &["n", "l"],
        &["n=0 l=[]"],
    );
}

// ----- bulk -----

/// `r -a-> m0, m1, m2 -b-> t -c-> u1 (x 1), u2 (x 2.5), u3 (no x)`: the three paths through
/// the `m`s merge at `t` (one traverser of bulk 3) before the last hop.
fn bulk_fixture() -> Arc<Graph> {
    let graph = Arc::new(Graph::new(TraversalGraph::new()));
    let _ = crate::script_host::eval_value(
        graph.clone(),
        r#"g.addV("r").property("name", "r").iterate();
           g.addV("t").property("name", "t").iterate();
           for i in 0..3 { g.addV("m").property("name", "m" + i).iterate(); }
           g.addV("u").property("name", "u1").property("x", 1).iterate();
           g.addV("u").property("name", "u2").property("x", 2.5).iterate();
           g.addV("u").property("name", "u3").iterate();
           g.V().hasLabel("r").as("r").V().hasLabel("m").addE("a").from("r").iterate();
           g.V().hasLabel("t").as("t").V().hasLabel("m").addE("b").to("t").iterate();
           g.V().hasLabel("t").as("t").V().hasLabel("u").addE("c").from("t").iterate();"#,
    )
    .unwrap();
    graph
}

#[test]
fn merged_rows_aggregate_like_unmerged_ones() {
    let graph = bulk_fixture();
    let graph = graph.read();
    let queries = [
        (
            "MATCH (:r)-[:a]->()-[:b]->()-[:c]->(u) RETURN u.name AS n, count(*) AS k, \
             count(u.x) AS cx, sum(u.x) AS s, avg(u.x) AS av, collect(u.x) AS l, \
             count(DISTINCT u) AS d, min(u.x) AS mi",
            vec!["n", "k", "cx", "s", "av", "l", "d", "mi"],
            vec![
                "n=u1 k=3 cx=3 s=3 av=1.0 l=[1, 1, 1] d=1 mi=1",
                "n=u2 k=3 cx=3 s=7.5 av=2.5 l=[2.5, 2.5, 2.5] d=1 mi=2.5",
                "n=u3 k=3 cx=0 s=0 av=null l=[] d=1 mi=null",
            ],
        ),
        (
            "MATCH (:r)-[:a]->()-[:b]->()-[:c]->(u) RETURN count(*) AS k, sum(u.x) AS s, \
             avg(u.x) AS av, collect(u.name) AS l, count(DISTINCT u.x) AS dx",
            vec!["k", "s", "av", "l", "dx"],
            vec!["k=9 s=10.5 av=1.75 l=[u1, u1, u1, u2, u2, u2, u3, u3, u3] dx=2"],
        ),
    ];
    for (query, keys, expected) in queries {
        let run = |merge: bool| {
            graph
                .traversal()
                .with("bulk.merge", merge)
                .gql(query)
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {e}"))
        };
        let merged = table(&run(true), &keys, false);
        assert_eq!(merged, table(&run(false), &keys, false), "{query}");
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(merged, expected, "{query}");
        // The merged run really fed fewer traversers into the barrier than rows.
        let text = profile(&graph, query);
        let line = text
            .lines()
            .find(|line| line.starts_with("gql:aggregate("))
            .unwrap_or_else(|| panic!("{text}"));
        let counts: Vec<&str> = line.split_whitespace().collect();
        assert!(counts.contains(&"3"), "{text}");
        assert!(!counts.contains(&"9"), "{text}");
    }
}

// ----- in the pipeline: RETURN DISTINCT / ORDER BY / OFFSET / LIMIT, WITH, MATCH after -----

#[test]
fn ordering_paging_and_distinct_after_the_aggregation() {
    let graph = TraversalGraph::tinkerpop_modern();
    check_ordered(
        &graph,
        "MATCH ()-[e]->() RETURN type(e) AS t, count(*) AS c ORDER BY c DESC, t",
        &["t", "c"],
        &["t=created c=4", "t=knows c=2"],
    );
    check_ordered(
        &graph,
        "MATCH ()-[e]->() RETURN type(e) AS t, count(*) AS c ORDER BY c DESC OFFSET 1 LIMIT 1",
        &["t", "c"],
        &["t=knows c=2"],
    );
    check_ordered(
        &graph,
        "MATCH (p:person)-->(x) RETURN p.name AS n, count(x) AS c ORDER BY c * -1, n LIMIT 2",
        &["n", "c"],
        &["n=marko c=3", "n=josh c=2"],
    );
    check(
        &graph,
        "MATCH (p:person) RETURN DISTINCT count(*) AS c",
        &["c"],
        &["c=4"],
    );
    let text = profile(
        &graph,
        "MATCH ()-[e]->() RETURN DISTINCT type(e) AS t, count(*) AS c ORDER BY c LIMIT 1",
    );
    let steps = steps(&text);
    let tail: Vec<&str> = steps
        .iter()
        .rev()
        .take(4)
        .rev()
        .map(|s| s.split('(').next().unwrap_or_default())
        .collect();
    assert_eq!(
        tail,
        ["gql:aggregate", "gql:distinct", "gql:order", "limit"],
        "{text}"
    );
    assert!(!text.contains("gql:project"), "{text}");
}

#[test]
fn the_plan_aggregates_in_place_of_the_projection() {
    let graph = TraversalGraph::tinkerpop_modern();
    let text = profile(
        &graph,
        "MATCH (p:person)-[:created]->(s) RETURN s.name AS sw, count(p) AS c, \
         collect(DISTINCT p.name) AS ps",
    );
    // The profile table cuts the step text at 64 characters.
    assert!(
        text.contains(r#"gql:aggregate(["s.name"], ["count(p)", "collect(DISTINCT "#),
        "{text}"
    );
    assert!(!text.contains("full"), "{text}");
    assert!(!text.contains("gql:project"), "{text}");
    // The aggregation reads `p` away from the current element: labelled, read via the label.
    assert!(text.contains(r#"as("p")"#), "{text}");
}

#[test]
fn aggregation_after_with_and_followed_by_match() {
    let graph = TraversalGraph::tinkerpop_modern();
    // An aggregating WITH, then a MATCH continuing from a key column.
    check(
        &graph,
        "MATCH (a:person)-[:knows]->(b) WITH a, count(b) AS n MATCH (a)-[:created]->(s) \
         RETURN a.name AS a, n, s.name AS s",
        &["a", "n", "s"],
        &["a=marko n=2 s=lop"],
    );
    let text = profile(
        &graph,
        "MATCH (a:person)-[:knows]->(b) WITH a, count(b) AS n MATCH (a)-[:created]->(s) \
         RETURN a.name AS a, n, s.name AS s",
    );
    assert!(
        text.contains(r#"gql:aggregate(["a"], ["count(b)"], ["a", "n"])"#),
        "{text}"
    );
    assert!(text.contains(r#"gql:bind("a")"#), "{text}");
    // Its WHERE filters the groups.
    check(
        &graph,
        "MATCH (p:person)-[:created]->(s) WITH s, count(p) AS c WHERE c > 1 RETURN s.name AS s, c",
        &["s", "c"],
        &["s=lop c=3"],
    );
    // An aggregation over the rows of an aggregating WITH.
    check(
        &graph,
        "MATCH (p:person)-[:created]->(s) WITH s, count(p) AS c RETURN sum(c) AS t, max(c) AS m, \
         collect(s.name) AS names",
        &["t", "m", "names"],
        &["t=4 m=3 names=[lop, ripple]"],
    );
    // An aggregation after a projecting WITH, LET and FILTER (substitutions).
    check(
        &graph,
        "MATCH (p:person) LET a = p.age FILTER a > 28 WITH p, a RETURN count(p) AS c, sum(a) AS s",
        &["c", "s"],
        &["c=3 s=96"],
    );
    // After a materializing WITH (ORDER BY / LIMIT).
    check(
        &graph,
        "MATCH (p:person) WITH p ORDER BY p.age DESC LIMIT 2 RETURN collect(p.name) AS ns, \
         avg(p.age) AS a",
        &["ns", "a"],
        &["ns=[josh, peter] a=33.5"],
    );
    // An aggregating WITH whose key continues into a MATCH that finds nothing for some groups.
    check(
        &graph,
        "MATCH (p:person)-[:created]->(s) WITH p, count(s) AS c MATCH (p)-[:knows]->(f) \
         RETURN p.name AS p, c, collect(f.name) AS fs",
        &["p", "c", "fs"],
        &["p=marko c=1 fs=[josh, vadas]"],
    );
    // After an OPTIONAL MATCH: an unmatched row counts for count(*) only.
    check(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS p, count(*) AS n, \
         count(s) AS c",
        &["p", "n", "c"],
        &[
            "p=josh n=2 c=2",
            "p=marko n=1 c=1",
            "p=peter n=1 c=1",
            "p=vadas n=1 c=0",
        ],
    );
}

// ----- twins and the large sample -----

mod differential {
    use super::*;

    /// Runs the GQL query and its Gremlin twin on `graph` and compares the rows as multisets.
    fn same(graph: &Arc<Graph>, query: &str, twin: &str) {
        let lock = graph.read();
        let left = lock
            .traversal()
            .gql(query)
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        drop(lock);
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, twin)
            .unwrap_or_else(|e| panic!("`{twin}` failed to evaluate: {e}"));
        let lock = graph.read();
        let right = gts
            .ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{twin}: {e}"));
        let text = |rows: &[ElementProperty]| -> Vec<String> {
            let mut out: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
            out.sort();
            out
        };
        assert_eq!(text(&left), text(&right), "\nGQL:  {query}\ntwin: {twin}");
        assert!(!left.is_empty(), "{query}: no rows to compare");
    }

    /// `group().by(key).by(value).unfold()` as rows `{k: key, v: value}`.
    fn grouped(start: &str, key: &str, value: &str, k: &str, v: &str) -> String {
        format!(
            r#"{start}.group().by({key}).by({value}).unfold().project("{k}", "{v}").by(__.select(Column.keys)).by(__.select(Column.values))"#
        )
    }

    #[test]
    fn grouped_aggregates_match_their_twins() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let cases = [
            (
                "MATCH (p:person)-[:created]->(s) RETURN s.name AS sw, count(p) AS c",
                grouped(
                    r#"g.V().hasLabel("person").out("created")"#,
                    r#""name""#,
                    "__.count()",
                    "sw",
                    "c",
                ),
            ),
            (
                "MATCH (p:person)-[:created]->(s) RETURN s, count(*) AS c",
                grouped(
                    r#"g.V().hasLabel("person").out("created")"#,
                    "__.identity()",
                    "__.count()",
                    "s",
                    "c",
                ),
            ),
            (
                "MATCH (p)-[e]->() RETURN p.name AS n, sum(e.weight) AS w",
                grouped(
                    "g.V().outE()",
                    r#"__.outV().values("name")"#,
                    r#"__.values("weight").sum()"#,
                    "n",
                    "w",
                ),
            ),
            (
                "MATCH (p)-[e]->() RETURN p.name AS n, max(e.weight) AS w",
                grouped(
                    "g.V().outE()",
                    r#"__.outV().values("name")"#,
                    r#"__.values("weight").max()"#,
                    "n",
                    "w",
                ),
            ),
            (
                "MATCH (p)-[e]->() RETURN type(e) AS t, min(e.weight) AS w",
                grouped(
                    "g.E()",
                    "__.label()",
                    r#"__.values("weight").min()"#,
                    "t",
                    "w",
                ),
            ),
        ];
        for (query, twin) in &cases {
            same(&graph, query, twin);
        }
        // Without keys: count() / fold() / mean() into one row.
        same(
            &graph,
            "MATCH (p:person) RETURN count(*) AS c",
            r#"g.V().hasLabel("person").count().project("c").by(__.identity())"#,
        );
        same(
            &graph,
            "MATCH (p:person) RETURN avg(p.age) AS a",
            r#"g.V().hasLabel("person").values("age").mean().project("a").by(__.identity())"#,
        );
        same(
            &graph,
            "MATCH (p:person) WHERE p.age > 30 RETURN collect(p.name) AS l",
            r#"g.V().hasLabel("person").has("age", P.gt(30)).values("name").fold().project("l").by(__.identity())"#,
        );
    }

    #[test]
    fn large_graph_aggregates_match_their_twins() {
        let graph = Arc::new(GraphSource::large_110k());
        same(
            &graph,
            "MATCH (v) RETURN v.level AS l, count(*) AS c",
            &grouped("g.V()", r#""level""#, "__.count()", "l", "c"),
        );
        same(
            &graph,
            "MATCH (a)-->(b) WHERE a.level = '2' RETURN count(*) AS c, count(DISTINCT b) AS d",
            r#"g.V().has("level", "2").out().fold().project("c", "d").by(__.count(Scope.local)).by(__.unfold().dedup().count())"#,
        );
        let lock = graph.read();
        let result = lock
            .traversal()
            .gql(
                "MATCH (a)-->(b) RETURN a.level AS l, count(*) AS c, count(DISTINCT b) AS d \
                 ORDER BY l",
            )
            .to_list()
            .unwrap();
        assert_eq!(
            table(&result, &["l", "c", "d"], true),
            [
                "l=0 c=100 d=100",
                "l=1 c=1000 d=1000",
                "l=2 c=10000 d=10000",
                "l=3 c=100000 d=100000",
            ]
        );
    }
}

// ----- GROUP BY (ISO GQL, stage 1c) -----

/// A written `GROUP BY` on `RETURN` names the keys implicit grouping uses anyway: same rows,
/// same plan as without it; without an aggregate it makes one row per group (`DISTINCT`).
#[test]
fn group_by_lists_the_implicit_keys() {
    let graph = TraversalGraph::tinkerpop_modern();
    let implicit = "MATCH (p:person)-[:created]->(s) RETURN p.name AS n, count(s) AS c";
    let expected = ["n=josh c=2", "n=marko c=1", "n=peter c=1"];
    for query in [
        implicit.to_string(),
        format!("{implicit} GROUP BY n"),
        format!("{implicit} GROUP BY p.name"),
        "MATCH (p:person)-[:created]->(s) RETURN p.name AS n, count(s) AS c group by n".into(),
    ] {
        check(&graph, &query, &["n", "c"], &expected);
        assert_eq!(
            steps(&profile(&graph, &query)),
            steps(&profile(&graph, implicit)),
            "{query}"
        );
    }
    // Unaliased items, a variable key, several keys, ORDER BY after GROUP BY.
    check(
        &graph,
        "MATCH (p:person)-[:created]->(s) RETURN p.name, count(s) GROUP BY p.name",
        &["p.name", "count(s)"],
        &[
            "p.name=josh count(s)=2",
            "p.name=marko count(s)=1",
            "p.name=peter count(s)=1",
        ],
    );
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (p:person)-[:created]->(s) RETURN s.lang AS l, p.age > 30 AS old, \
                 count(*) AS c GROUP BY l, old ORDER BY c DESC, old"
            ),
            &["l", "old", "c"],
            true
        ),
        ["l=java old=true c=3", "l=java old=false c=1"]
    );
    // The empty grouping set: one group.
    check(
        &graph,
        "MATCH (p:person)-[:created]->(s) RETURN count(s) AS c GROUP BY ()",
        &["c"],
        &["c=4"],
    );
    // No aggregate: one row per group (lop has three creators).
    let query = "MATCH (p:person)-[:created]->(s) RETURN s.name AS s GROUP BY s";
    check(&graph, query, &["s"], &["s=lop", "s=ripple"]);
    assert_eq!(
        steps(&profile(&graph, query)),
        steps(&profile(
            &graph,
            "MATCH (p:person)-[:created]->(s) RETURN DISTINCT s.name AS s"
        ))
    );
    check(
        &graph,
        "MATCH (p:person)-[:created]->(s) RETURN * GROUP BY p, s",
        &["p"],
        &["p=v[1]", "p=v[4]", "p=v[4]", "p=v[6]"],
    );
}

#[test]
fn group_by_keys_must_be_the_items_without_an_aggregate() {
    let graph = TraversalGraph::tinkerpop_modern();
    for (query, text) in [
        (
            "MATCH (p:person)-[:created]->(s) RETURN p.name AS n, count(s) AS c GROUP BY p",
            "`p` is not a RETURN item",
        ),
        (
            "MATCH (p:person)-[:created]->(s) RETURN p.name AS n, count(s) AS c GROUP BY c",
            "`c` is an aggregating RETURN item",
        ),
        (
            "MATCH (p:person)-[:created]->(s) RETURN p.name AS n, p.age AS a, count(s) AS c \
             GROUP BY n",
            "the RETURN item `a` has no aggregate, so it is a grouping key, and GROUP BY does \
             not list it",
        ),
        (
            "MATCH (p:person)-[:created]->(s) RETURN p.name AS n, count(s) AS c GROUP BY ()",
            "GROUP BY () groups all rows into one",
        ),
    ] {
        let (status, message) = gql_error(&graph, query);
        assert_eq!(status, "42002", "{query}");
        assert!(message.contains(text), "{query}: {message}");
    }
}

// ----- percentiles (stage 3f) -----

#[test]
fn percentiles_on_the_modern_graph() {
    let graph = TraversalGraph::tinkerpop_modern();
    // Ages 27, 29, 32, 35 (hand-computed with Neo4j's definitions).
    check(
        &graph,
        "MATCH (p:person) RETURN percentile_disc(p.age, 0.5) AS d, \
         percentile_cont(p.age, 0.5) AS c, percentileDisc(p.age, 0) AS d0, \
         percentileCont(p.age, 1) AS c1, percentileCont(p.age, 0.25) AS q",
        &["d", "c", "d0", "c1", "q"],
        &["d=29 c=30.5 d0=27 c1=35.0 q=28.5"],
    );
    // Grouped, NULL skipped (software has no age), a parameter as the percentile.
    let rows = graph
        .traversal()
        .gql_with(
            "MATCH (n) RETURN n.lang AS l, percentileCont(n.age, $p) AS c",
            [("p", 0.5)],
        )
        .to_list()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        table(&rows, &["l", "c"], false),
        ["l=java c=null", "l=null c=30.5"]
    );
    // DISTINCT, and an aggregating WITH before a filter.
    check(
        &graph,
        "MATCH (p:person)-[:knows|created]->(x) WITH percentile_disc(DISTINCT p.age, 1.0) AS d \
         WHERE d > 30 RETURN d",
        &["d"],
        &["d=35"],
    );
}

#[test]
fn percentiles_aggregate_merged_rows_like_unmerged_ones() {
    let graph = bulk_fixture();
    let graph = graph.read();
    let query = "MATCH (:r)-[:a]->()-[:b]->()-[:c]->(u) RETURN percentileDisc(u.x, 0.5) AS d, \
                 percentileCont(u.x, 0.4) AS c";
    let run = |merge: bool| {
        graph
            .traversal()
            .with("bulk.merge", merge)
            .gql(query)
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"))
    };
    let merged = table(&run(true), &["d", "c"], false);
    assert_eq!(merged, table(&run(false), &["d", "c"], false));
    // 1, 1, 1, 2.5, 2.5, 2.5: disc at ceil(3) - 1 = 2, cont at 0.4 * 5 = 2.
    assert_eq!(merged, ["d=1 c=1.0"]);
}
