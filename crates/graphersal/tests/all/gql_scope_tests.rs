//! GQL binder scope and grouping (stage 3f, Cypher compatibility forms measured against the
//! openCypher TCK): ISO non-reserved words as variable names (`Unwind1`, `Match4`), the "not
//! yet" of a quantified edge over a bound list of edges, `WITH *` with nothing in scope and
//! `[:A|:B]` (`Create3`, `Match3`), the `WHERE` of a `WITH` reading the variables before it
//! (`WithWhere1`, `WithWhere7`, `TriadicSelection1`), `ORDER BY` over expressions of the
//! returned aggregates and keys (`ReturnOrderBy6`, `WithOrderBy4`), and a `LET` / `WITH` value
//! used as a pattern node (`Match7`, `Path1`, `Path2`). Hand-computed rows on the modern graph,
//! rewrite pairs, the plans of queries the changes must leave alone, and the errors.
#![cfg(feature = "gql")]

use std::sync::Arc;

use graphersal::prelude::*;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn rows(graph: &Arc<Graph>, query: &str) -> Vec<ElementProperty> {
    graph
        .read()
        .traversal()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

/// The failure of `query`: the message of its GQL root cause and its GQLSTATUS.
fn failure(graph: &Arc<Graph>, query: &str) -> String {
    match graph.read().traversal().gql(query).to_list() {
        Ok(rows) => panic!("{query} gave {rows:?}"),
        Err(error) => match error.root_cause() {
            TraverserError::Gql {
                message, gqlstatus, ..
            } => format!("{message} [{gqlstatus}]"),
            other => panic!("{query}: not a GQL error: {other}"),
        },
    }
}

/// The steps of the profile of `query`, without the metrics columns.
fn steps(graph: &Arc<Graph>, query: &str) -> Vec<String> {
    let profile = graph
        .read()
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
        .filter(|line| !line.trim_start().starts_with('[') && !line.contains("TOTAL:"))
        .map(|line| {
            let step = line.trim_start();
            step[..step.find("  ").unwrap_or(step.len())].to_string()
        })
        .collect()
}

/// A value as comparable text: `null`, `'text'`, numbers, `[..]`, `v<id>`, `e<id>`.
fn norm(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => format!("'{s}'"),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) if n.is_i64() => i.to_string(),
            (_, Some(f)) => format!("{f:?}"),
            _ => format!("{n:?}"),
        },
        ElementProperty::Array(items) => {
            let items: Vec<String> = items.iter().map(norm).collect();
            format!("[{}]", items.join(", "))
        }
        ElementProperty::Vertex(v) => format!("v{}", v.id()),
        ElementProperty::Edge(e) => format!("e{}", e.id()),
        other => format!("{other:?}"),
    }
}

/// Every row of `query` as `a|b|..` over its columns `keys`, in result order.
fn ordered(graph: &Arc<Graph>, query: &str, keys: &[&str]) -> Vec<String> {
    rows(graph, query)
        .iter()
        .map(|row| match row {
            ElementProperty::Object(map) => keys
                .iter()
                .map(|k| norm(map.get(k).unwrap_or(&ElementProperty::Null)))
                .collect::<Vec<_>>()
                .join("|"),
            other => panic!("{query}: not a row map: {other:?}"),
        })
        .collect()
}

/// [`ordered`], sorted (a multiset).
fn table(graph: &Arc<Graph>, query: &str, keys: &[&str]) -> Vec<String> {
    let mut out = ordered(graph, query, keys);
    out.sort();
    out
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

// ----- non-reserved words as names -----

#[test]
fn non_reserved_words_are_variable_names() {
    let graph = modern();
    // TCK Unwind1 [3].
    assert_eq!(
        table(
            &graph,
            "WITH [1, 2] AS first, [3, 4] AS second UNWIND (first + second) AS x RETURN x",
            &["x"]
        ),
        sorted(&["1", "2", "3", "4"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (first:person {name: 'marko'})-[last:created]->(shortest) \
             RETURN first.name AS a, last.weight AS w, shortest.name AS b",
            &["a", "w", "b"]
        ),
        sorted(&["'marko'|0.4|'lop'"])
    );
    // The same query with ordinary names gives the same rows.
    assert_eq!(
        table(
            &graph,
            "MATCH (f:person)-[l:created]->(s) RETURN f.name AS a, s.name AS b",
            &["a", "b"]
        ),
        table(
            &graph,
            "MATCH (first:person)-[last:created]->(labeled) RETURN first.name AS a, \
             labeled.name AS b",
            &["a", "b"]
        )
    );
    // A name never changes the plan.
    assert_eq!(
        steps(
            &graph,
            "MATCH (first:person)-->(last) RETURN last.name AS n"
        ),
        steps(&graph, "MATCH (a:person)-->(b) RETURN b.name AS n")
    );
}

#[test]
fn a_quantified_edge_over_a_bound_list_of_edges_is_not_yet() {
    let graph = modern();
    let message = failure(
        &graph,
        "MATCH ()-[r1]->()-[r2]->() WITH [r1, r2] AS rs LIMIT 1 \
         MATCH (first)-[rs*]->(second) RETURN first, second",
    );
    assert_eq!(
        message,
        "a quantified edge over a list of edges bound before is not yet supported in this \
         version [0A000]"
    );
    // The advice's rewrite runs.
    assert_eq!(
        table(
            &graph,
            "MATCH (a)-[r1]->()-[r2]->(b) WITH [r1, r2] AS rs, a AS x, b AS y \
             MATCH (x)-[rs2*]->(y) WHERE rs2 = rs RETURN x.name AS x, y.name AS y",
            &["x", "y"]
        ),
        sorted(&["'marko'|'lop'", "'marko'|'ripple'"])
    );
}

// ----- singles: WITH * with nothing in scope, `:A|:B` -----

#[test]
fn with_star_with_nothing_in_scope_passes_the_rows_on() {
    let graph = modern();
    // TCK Create3 [2]: the rows of an anonymous MATCH pass through WITH *.
    assert_eq!(
        table(&graph, "MATCH () WITH * RETURN count(*) AS c", &["c"]),
        sorted(&["6"])
    );
    assert_eq!(
        table(&graph, "WITH * RETURN 1 AS x", &["x"]),
        sorted(&["1"])
    );
    // A materializing WITH * keeps the rows too; DISTINCT folds them into one empty record.
    assert_eq!(
        table(
            &graph,
            "MATCH () WITH * LIMIT 2 RETURN count(*) AS c",
            &["c"]
        ),
        sorted(&["2"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH () WITH DISTINCT * RETURN count(*) AS c",
            &["c"]
        ),
        sorted(&["1"])
    );
    // A write after it runs once per row.
    let graph = modern();
    let created = graph
        .write()
        .traversal_mut()
        .gql("MATCH (:software) CREATE () WITH * CREATE (:new) RETURN 1 AS x")
        .to_list()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(created.len(), 2);
    assert_eq!(
        table(&graph, "MATCH (n:new) RETURN count(*) AS c", &["c"]),
        sorted(&["2"])
    );
    // RETURN * still needs a variable.
    assert_eq!(
        failure(&modern(), "MATCH () RETURN *"),
        "RETURN * needs at least one named variable, and this query has none [42002]"
    );
}

#[test]
fn a_colon_after_the_bar_of_an_edge_label_disjunction_is_allowed() {
    let graph = modern();
    // TCK Match3 [8]: `:T|:T` is `:T|T`.
    for (cypher, gql) in [
        (
            "MATCH (a)-[:knows|:created]->(b) RETURN b.name AS n",
            "MATCH (a)-[:knows|created]->(b) RETURN b.name AS n",
        ),
        (
            "MATCH (a)-[r:knows|:knows]->(b) RETURN b.name AS n",
            "MATCH (a)-[r:knows|knows]->(b) RETURN b.name AS n",
        ),
    ] {
        assert_eq!(
            table(&graph, cypher, &["n"]),
            table(&graph, gql, &["n"]),
            "{cypher}"
        );
        assert_eq!(steps(&graph, cypher), steps(&graph, gql), "{cypher}");
    }
}
