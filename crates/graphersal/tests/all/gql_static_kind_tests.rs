//! GQL stage 3f, the openCypher TCK alignment of the user decisions U4, U9-U12: the static kind
//! checks (an operand whose kind is certain from the query text and that its place never
//! takes is refused before the traversal runs, also on an empty graph), a pattern predicate in
//! a value position (42001), `IN` with a right operand that is not a list (22G03 when a row
//! reaches it), `toString` of a list or map (22G03, `toStringOrNull` NULL), a comprehension
//! variable reusing a name that is no longer in scope, and `RETURN *` / `WITH *` in name order.
#![cfg(feature = "gql")]

use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;

fn run(graph: &TraversalGraph, query: &str) -> Result<Vec<ElementProperty>, Box<TraverserError>> {
    graph.traversal().gql(query).to_list()
}

/// The failure of `query`: its GQLSTATUS, its kind, and whether it arose before the run (the
/// `gql:error` marker).
fn failure(graph: &TraversalGraph, query: &str) -> (String, GqlErrorKind, bool) {
    let error = match run(graph, query) {
        Ok(rows) => panic!("{query} gave {rows:?}"),
        Err(error) => error,
    };
    let compile_time = error
        .to_string()
        .contains("refused the query before the traversal ran");
    match error.root_cause() {
        TraverserError::Gql {
            kind, gqlstatus, ..
        } => (gqlstatus.clone(), *kind, compile_time),
        other => panic!("{query}: not a GQL error: {other}"),
    }
}

/// The column names of the first row of `query`.
fn columns(graph: &TraversalGraph, query: &str) -> Vec<String> {
    let rows = run(graph, query).unwrap_or_else(|e| panic!("{query}: {e}"));
    match rows.first() {
        Some(ElementProperty::Map(map)) => map
            .keys()
            .map(|k| k.as_string().unwrap_or_default().to_string())
            .collect(),
        Some(ElementProperty::Object(map)) => map.keys().map(|k| k.to_string()).collect(),
        other => panic!("{query}: {other:?}"),
    }
}

#[test]
fn operands_of_a_certain_wrong_kind_are_refused_before_the_run() {
    let empty = TraversalGraph::new();
    let modern = TraversalGraph::tinkerpop_modern();
    for query in [
        "RETURN false AND 123 AS x",
        "RETURN true OR [] AS x",
        "RETURN 'a' XOR true AS x",
        "RETURN NOT 'x' AS x",
        "RETURN NOT {} AS x",
        "RETURN 1 IN true AS x",
        "RETURN 1 IN {x: []} AS x",
        "MATCH (r) RETURN type(r) AS t",
        "MATCH (n) RETURN length(n) AS l",
        "MATCH ()-[r]->() RETURN length(r) AS l",
        "MATCH p = (a)-[*]->(b) RETURN size(p) AS s",
        "MATCH (n) MATCH r = (n)-[*]->() WHERE r.name = 'apa' RETURN r",
        "MATCH (n) WHERE (n) RETURN n",
        "MATCH p = (a) RETURN labels(p) AS l",
        "RETURN properties(1) AS p",
        "RETURN properties([true, false]) AS p",
        "WITH 123 AS x RETURN x.num AS y",
        "WITH [123, true] AS x RETURN x.num AS y",
        "LET x = 'string' RETURN x.num AS y",
        "RETURN none(x IN ['Clara'] WHERE x % 2 = 0) AS r",
        "RETURN all(x IN [false, true] WHERE x % 2 = 0) AS r",
        "RETURN [x IN [1, 2] WHERE x] AS l",
        "RETURN CASE WHEN 1 THEN 2 END AS c",
        "MATCH (n) WHERE n.age > 1 AND 'yes' RETURN n",
        "RETURN - 'a' AS x",
        "MATCH (n) RETURN size(nodes((n)-->()) ) AS s",
    ] {
        for graph in [&empty, &modern] {
            let (status, _, compile_time) = failure(graph, query);
            assert!(compile_time, "{query}: not refused before the run");
            if query.contains("(n)-->()") {
                assert_eq!(status, "42001", "{query}");
            } else {
                assert_eq!(status, "22G03", "{query}");
            }
        }
    }
}

#[test]
fn uncertain_kinds_keep_the_run_time_check() {
    let empty = TraversalGraph::new();
    let modern = TraversalGraph::tinkerpop_modern();
    for query in [
        // A property value, a parameter, a mixed list, a NULL literal, a subscript, a
        // comprehension over a literal of another kind.
        "MATCH (n) RETURN n.name AND true AS x",
        "MATCH (n) RETURN n.name IN n.age AS x",
        "RETURN any(x IN [1, 'a'] WHERE x % 2 = 0) AS r",
        "WITH 'abc' AS s RETURN s[0] AS x",
        "RETURN [x IN 5 | x] AS l",
    ] {
        let (status, kind, compile_time) = failure(&modern, query);
        assert_eq!(
            (status.as_str(), kind),
            ("22G03", GqlErrorKind::Data),
            "{query}"
        );
        assert!(!compile_time, "{query}: refused before the run");
    }
    // On the empty graph no row reaches a property read.
    assert_eq!(
        run(&empty, "MATCH (n) RETURN n.name AND true AS x").unwrap(),
        []
    );
    // A NULL operand and an empty list are never refused.
    for query in [
        "RETURN NULL AND TRUE AS a, NULL IN NULL AS b, 'a' % NULL AS c",
        "RETURN none(x IN [] WHERE x % 2 = 0) AS r",
        "RETURN [x IN [1, NULL] WHERE x > 0 | x * 2] AS l",
        "MATCH p = (a)-[e]->(b) RETURN type(e) AS t, labels(a) AS l, length(p) AS n, size(nodes(p)) AS s LIMIT 1",
    ] {
        run(&modern, query).unwrap_or_else(|e| panic!("{query}: {e}"));
    }
    // A parameter's value is never looked at: the error comes from the row.
    let result = modern
        .traversal()
        .gql_with("RETURN 1 IN $p AS x", [("p", ElementProperty::from(3))])
        .to_list();
    let error = result.expect_err("IN of an integer");
    assert!(
        !error
            .to_string()
            .contains("refused the query before the traversal ran"),
        "{error}"
    );
    let ok = modern
        .traversal()
        .gql_with("RETURN 1 IN $p AS x", [("p", ElementProperty::Null)])
        .to_list()
        .unwrap();
    assert_eq!(ok.len(), 1);
}

#[test]
fn in_with_a_right_operand_that_is_no_list_fails_when_a_row_reaches_it() {
    let modern = TraversalGraph::tinkerpop_modern();
    let (status, kind, compile_time) = failure(
        &modern,
        "MATCH (n:person {name: 'marko'}) RETURN 1 IN n.age AS x",
    );
    assert_eq!(
        (status.as_str(), kind, compile_time),
        ("22G03", GqlErrorKind::Data, false)
    );
    // A NULL list is NULL, a missing property too.
    let rows = run(
        &modern,
        "MATCH (n:person {name: 'marko'}) RETURN 1 IN n.tags IS NULL AS x",
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
}

#[test]
fn a_pattern_predicate_stands_only_where_a_condition_does() {
    let modern = TraversalGraph::tinkerpop_modern();
    for query in [
        "MATCH (n) RETURN (n)-[]->() AS x",
        "MATCH (n) WITH (n)-[]->() AS x RETURN x",
        "MATCH (a) RETURN size((a)-->()) AS s",
        "MATCH (n) SET n.prop = head(nodes(head((n)-[:REL]->()))).foo",
    ] {
        let (status, kind, compile_time) = failure(&modern, query);
        assert_eq!(
            (status.as_str(), kind, compile_time),
            ("42001", GqlErrorKind::Syntax, true)
        );
    }
    // The rewrites the advice names give the values.
    let rows = run(
        &modern,
        "MATCH (p:person {name: 'marko'}) RETURN EXISTS { (p)-[:knows]->() } AS e, \
         COUNT { (p)-[:knows]->() } AS c, size([(p)-[:knows]->(f) | f]) AS s",
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    let rows = run(
        &modern,
        "MATCH (p:person) WHERE NOT (p)-[:created]->() OR (p)<-[:knows]-() RETURN p.name AS n",
    )
    .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
}

#[test]
fn to_string_takes_scalars_only() {
    let modern = TraversalGraph::tinkerpop_modern();
    for query in [
        "MATCH p = (n)-[r:knows]->() RETURN [x IN [1, '', []] | toString(x)] AS l",
        "MATCH p = (n)-[r:knows]->() RETURN [x IN [1, '', {}] | toString(x)] AS l",
        "MATCH p = (n)-[r:knows]->() RETURN toString(n) AS l",
    ] {
        let (status, _, compile_time) = failure(&modern, query);
        assert_eq!((status.as_str(), compile_time), ("22G03", false), "{query}");
    }
    let rows = run(
        &modern,
        "MATCH p = (n)-[r:knows]->() RETURN toStringOrNull([]) IS NULL AS a, \
         toStringOrNull(n) IS NULL AS b, toStringOrNull(p) IS NULL AS c, \
         CAST([1, 'a'] AS STRING) AS d LIMIT 1",
    )
    .unwrap();
    let text = format!("{rows:?}");
    assert!(!text.contains("false"), "{text}");
    assert!(text.contains("[1, a]"), "{text}");
}

#[test]
fn a_comprehension_variable_may_reuse_a_name_out_of_scope() {
    let modern = TraversalGraph::tinkerpop_modern();
    let rows = run(
        &modern,
        "MATCH (x:person) WITH collect(x) AS xs RETURN none(x IN xs WHERE x.age > 40) AS r",
    )
    .unwrap();
    assert_eq!(format!("{rows:?}").matches("true").count(), 1, "{rows:?}");
    // A name a LET / WITH value in scope still reads (or a renamed variable) stays refused.
    for query in [
        "MATCH (n:person) WITH n AS m RETURN [n IN [1] | m] AS l",
        "MATCH (n:person) WITH n.name AS m RETURN [n IN [1] | m] AS l",
        "MATCH (n:person) RETURN [n IN [1] | n] AS l",
    ] {
        let (status, kind, _) = failure(&modern, query);
        assert_eq!(
            (status.as_str(), kind),
            ("42002", GqlErrorKind::Semantic),
            "{query}"
        );
    }
}

#[test]
fn star_columns_come_in_name_order() {
    let modern = TraversalGraph::tinkerpop_modern();
    assert_eq!(
        columns(
            &modern,
            "MATCH p = (a:person)-[:created]->(b) RETURN * LIMIT 1"
        ),
        ["a", "b", "p"]
    );
    assert_eq!(
        columns(
            &modern,
            "UNWIND [1] AS zs UNWIND [2] AS z UNWIND [3] AS x RETURN *"
        ),
        ["x", "z", "zs"]
    );
    assert_eq!(
        columns(
            &modern,
            "MATCH (n:person {name: 'marko'}) WITH n, 1 AS a WITH * RETURN *, 2 AS b"
        ),
        ["a", "n", "b"]
    );
}
