//! GQL parser forms of stage 3f (Cypher compatibility forms measured against the openCypher TCK):
//! chained comparisons `a < b < c` as `a < b AND b < c` (`Comparison3`, `Comparison4`), Cypher's
//! empty variable-length interval `[*2..1]` / `[*..0]` matching nothing (`Match5` [11]-[13])
//! while the GQL `{3,1}` stays a syntax error, and constant `OFFSET` / `SKIP` / `LIMIT`
//! expressions evaluated once (`ReturnSkipLimit2` [6]). Rewrite pairs on the modern graph,
//! the plans of the rewrites, and the errors.
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

fn column<'r>(row: &'r ElementProperty, key: &str) -> &'r ElementProperty {
    match row {
        ElementProperty::Object(map) => map
            .get(key)
            .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
        other => panic!("not a row map: {other:?}"),
    }
}

// ----- chained comparisons -----

#[test]
fn a_chained_comparison_is_the_conjunction_of_its_links() {
    let g = modern();
    for (chain, conjunction) in [
        (
            "MATCH (p:person) WHERE 27 <= p.age < 32 RETURN p.name AS n",
            "MATCH (p:person) WHERE 27 <= p.age AND p.age < 32 RETURN p.name AS n",
        ),
        (
            "MATCH (p:person) WHERE 30 > p.age >= 27 <> 29 RETURN p.name AS n",
            "MATCH (p:person) WHERE 30 > p.age AND p.age >= 27 AND 27 <> 29 RETURN p.name AS n",
        ),
        (
            "MATCH (p:person) RETURN p.name AS n, 1 < p.age + 1 = 30 AS x",
            "MATCH (p:person) RETURN p.name AS n, 1 < p.age + 1 AND p.age + 1 = 30 AS x",
        ),
        (
            "MATCH (p:person) RETURN p.name AS n, p.age < 30 < p.age AS x",
            "MATCH (p:person) RETURN p.name AS n, p.age < 30 AND 30 < p.age AS x",
        ),
    ] {
        assert_eq!(rows(&g, chain), rows(&g, conjunction), "{chain}");
        assert_eq!(steps(&g, chain), steps(&g, conjunction), "{chain}");
    }
}

#[test]
fn chained_comparisons_follow_three_valued_logic() {
    let g = modern();
    let row = rows(
        &g,
        "RETURN 1 < 2 < 3 AS a, 1 < 3 < 2 AS b, 1 < 2 < 3 < 4 < 0 AS c, NULL < 1 < 2 AS d, \
         2 < 1 < NULL AS e, 1 = 1 = 1 AS f, 1 < 2 < 3 OR false AS g",
    );
    let row = &row[0];
    let truth = |key: &str| column(row, key).clone();
    assert_eq!(truth("a"), ElementProperty::Boolean(true));
    assert_eq!(truth("b"), ElementProperty::Boolean(false));
    assert_eq!(truth("c"), ElementProperty::Boolean(false));
    assert_eq!(truth("d"), ElementProperty::Null);
    // FALSE AND NULL is FALSE.
    assert_eq!(truth("e"), ElementProperty::Boolean(false));
    assert_eq!(truth("f"), ElementProperty::Boolean(true));
    assert_eq!(truth("g"), ElementProperty::Boolean(true));
    // The column of an unaliased chain is its whole text.
    let row = rows(&g, "RETURN 1 < 2 <= 2");
    assert_eq!(
        column(&row[0], "1 < 2 <= 2"),
        &ElementProperty::Boolean(true)
    );
}

#[test]
fn a_chain_may_hold_a_subquery_in_its_middle() {
    let g = modern();
    let row = rows(&g, "RETURN 0 < COUNT { MATCH (n) } <= 6 AS x");
    assert_eq!(column(&row[0], "x"), &ElementProperty::Boolean(true));
}

// ----- the empty Cypher interval -----

#[test]
fn an_empty_cypher_interval_matches_nothing() {
    let g = modern();
    for query in [
        "MATCH (a)-[:knows*2..1]->(c) RETURN c",
        "MATCH (a)-[*1..0]->(c) RETURN c",
        "MATCH (a)-[e*..0]->(c) RETURN c, e",
        "MATCH p = (a)-[*3..2]->(c) RETURN p",
        "MATCH (a) MATCH (a)-[*2..1]->(c) RETURN count(*) AS n",
    ] {
        let found = rows(&g, query);
        let empty = query.contains("count(*)");
        if empty {
            assert_eq!(
                column(&found[0], "n"),
                &ElementProperty::from(0i64),
                "{query}"
            );
        } else {
            assert!(found.is_empty(), "{query}: {found:?}");
        }
    }
    // OPTIONAL MATCH: every row gets its NULL.
    let found = rows(
        &g,
        "MATCH (a:person) OPTIONAL MATCH (a)-[*3..1]->(b) RETURN a.name AS n, b",
    );
    assert_eq!(found.len(), 4);
    assert!(
        found
            .iter()
            .all(|r| column(r, "b") == &ElementProperty::Null)
    );
    // A pattern predicate over it is false.
    let found = rows(
        &g,
        "MATCH (a:person) WHERE NOT EXISTS { (a)-[*2..1]->() } RETURN a.name AS n",
    );
    assert_eq!(found.len(), 4);
}

#[test]
fn the_gql_quantifier_keeps_the_iso_syntax_error() {
    let g = modern();
    assert_eq!(
        failure(&g, "MATCH (a)-[e]->{3,1}(b) RETURN b"),
        "the quantifier's lower bound 3 is greater than its upper bound 1 [42001]"
    );
}

#[test]
fn a_non_empty_cypher_interval_is_unchanged() {
    let g = modern();
    assert_eq!(
        steps(&g, "MATCH (a)-[*1..2]->(c) RETURN c"),
        steps(&g, "MATCH (a)-[]->{1,2}(c) RETURN c")
    );
    assert_eq!(rows(&g, "MATCH (a)-[*1..1]->(c) RETURN c").len(), 6);
}

// ----- constant OFFSET / LIMIT -----

#[test]
fn a_constant_offset_or_limit_is_evaluated_once() {
    let g = modern();
    for (expression, literal) in [
        (
            "MATCH (p:person) RETURN p.name AS n ORDER BY n SKIP 1 + 1 LIMIT 2 * 1",
            "MATCH (p:person) RETURN p.name AS n ORDER BY n SKIP 2 LIMIT 2",
        ),
        (
            "MATCH (p:person) RETURN p.name AS n ORDER BY n LIMIT toInteger(ceil(1.7))",
            "MATCH (p:person) RETURN p.name AS n ORDER BY n LIMIT 2",
        ),
        (
            "MATCH (p:person) WITH p ORDER BY p.name OFFSET size(['x']) RETURN p.name AS n",
            "MATCH (p:person) WITH p ORDER BY p.name OFFSET 1 RETURN p.name AS n",
        ),
        (
            "MATCH (p:person) RETURN p.name AS n ORDER BY n LIMIT CASE WHEN 1 < 2 THEN 3 END",
            "MATCH (p:person) RETURN p.name AS n ORDER BY n LIMIT 3",
        ),
    ] {
        assert_eq!(rows(&g, expression), rows(&g, literal), "{expression}");
        assert_eq!(steps(&g, expression), steps(&g, literal), "{expression}");
    }
}

#[test]
fn a_constant_offset_or_limit_must_be_a_non_negative_integer() {
    let g = modern();
    for (query, expected) in [
        (
            "MATCH (p) RETURN p LIMIT 1 - 2",
            "LIMIT must be a non-negative integer, a parameter or a constant expression, not a \
             negative number [22G02]",
        ),
        (
            "MATCH (p) RETURN p SKIP 3 / 2.0",
            "OFFSET must be a non-negative integer, a parameter or a constant expression, not a \
             float [22G03]",
        ),
        (
            "MATCH (p) RETURN p LIMIT 'a' + 'b'",
            "LIMIT must be a non-negative integer, a parameter or a constant expression, not a \
             string [22G03]",
        ),
        (
            "MATCH (p) RETURN p LIMIT toInteger('x')",
            "LIMIT must be a non-negative integer, a parameter or a constant expression, not \
             null [22G03]",
        ),
        (
            "MATCH (p) RETURN p LIMIT p.age",
            "LIMIT must be a non-negative integer, a parameter or a constant expression, not an \
             expression that reads the row [22G03]",
        ),
        (
            "MATCH (p) RETURN p LIMIT COUNT { MATCH (x) }",
            "LIMIT must be a non-negative integer, a parameter or a constant expression, not an \
             expression that reads the row [22G03]",
        ),
        ("MATCH (p) RETURN p LIMIT 1 / 0", "division by zero [22012]"),
    ] {
        assert_eq!(failure(&g, query), expected, "{query}");
    }
    // A parameter inside an expression is no constant (parameters are bound per call).
    let error = g
        .read()
        .traversal()
        .gql_with(
            "MATCH (p) RETURN p LIMIT $n + 1",
            [("n", ElementProperty::from(1i64))],
        )
        .to_list()
        .map(|_| ())
        .map_err(|e| e.to_string());
    assert!(
        error.as_ref().is_err_and(|e| e.contains("22G03")),
        "{error:?}"
    );
}
