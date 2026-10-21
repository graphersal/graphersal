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

// ----- ORDER BY over expressions of returned aggregates and keys (B7) -----

#[test]
fn order_by_reads_aggregates_and_keys_written_inside_an_expression() {
    let graph = modern();
    // TCK ReturnOrderBy6 [1] / WithOrderBy4 [16]: a constant around the returned aggregate.
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN avg(p.age) AS a ORDER BY 1 + avg(p.age) - 1000",
            &["a"]
        ),
        ["30.75"]
    );
    // ReturnOrderBy6 [2] / [3]: an alias or a key expression together with the aggregate;
    // `p.name + toString(count(x))` is the key `n` and the aggregate `c` (peter1, marko3,
    // josh2 descending).
    for query in [
        "MATCH (p:person)-->(x) RETURN p.name AS n, count(x) AS c \
         ORDER BY p.name + toString(count(x)) DESC",
        "MATCH (p:person)-->(x) RETURN p.name AS n, count(x) AS c ORDER BY n + toString(c) DESC",
        "MATCH (p:person)-->(x) WITH p.name AS n, count(x) AS c \
         ORDER BY n + toString(count(x)) DESC RETURN n, c",
    ] {
        assert_eq!(
            ordered(&graph, query, &["n", "c"]),
            ["'peter'|1", "'marko'|3", "'josh'|2"],
            "{query}"
        );
    }
    // WithOrderBy2 [23]: the grouping key `p.name` inside the sort key, with LIMIT.
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person)-->(x) WITH p.name AS n, count(*) AS c \
             ORDER BY p.name + 'C' DESC LIMIT 1 RETURN n, c",
            &["n", "c"]
        ),
        ["'peter'|1"]
    );
    // Unaliased RETURN items, and DISTINCT columns.
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person)-->(x) RETURN p.name, count(*) ORDER BY size(p.name) * 10 - count(*)",
            &["p.name", "count(*)"]
        ),
        ["'josh'|2", "'marko'|3", "'peter'|1"]
    );
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN DISTINCT p.age AS a ORDER BY -p.age",
            &["a"]
        ),
        ["35", "32", "29", "27"]
    );
    // What no returned column holds stays refused.
    assert_eq!(
        failure(
            &graph,
            "MATCH (p:person)-->(x) RETURN count(*) AS c ORDER BY p.age + count(*)"
        ),
        "after an aggregation, ORDER BY can only use the returned columns, and `p` is not one \
         of them [42002]"
    );
    assert_eq!(
        failure(
            &graph,
            "MATCH (p:person) RETURN count(*) AS c ORDER BY max(p.age) + 1"
        ),
        "ORDER BY can only use an aggregate that RETURN computes [42002]"
    );
}

#[test]
fn an_order_by_key_written_like_an_item_reads_its_column() {
    let graph = modern();
    // The same rows and the same steps as the alias spelling (the rendering keeps the text).
    let written = "MATCH (p:person)-->(x) RETURN p.name AS n, count(x) AS c \
                   ORDER BY 1 - count(x), p.name";
    let aliased = "MATCH (p:person)-->(x) RETURN p.name AS n, count(x) AS c ORDER BY 1 - c, n";
    assert_eq!(
        ordered(&graph, written, &["n", "c"]),
        ["'marko'|3", "'josh'|2", "'peter'|1"]
    );
    assert_eq!(
        ordered(&graph, written, &["n", "c"]),
        ordered(&graph, aliased, &["n", "c"])
    );
    let (w, a) = (steps(&graph, written), steps(&graph, aliased));
    assert_eq!(w.len(), a.len());
    assert_eq!(w[..w.len() - 1], a[..a.len() - 1]);
    assert_eq!(
        w.last().map(String::as_str),
        Some("gql:order(\"1 - count(x)\", \"p.name\")")
    );
}

// ----- a LET / WITH value as a pattern element (B11) -----

#[test]
fn a_value_holding_a_node_or_an_edge_continues_a_pattern() {
    let graph = modern();
    // TCK Match7 [22]: a computed node continues the pattern.
    assert_eq!(
        table(
            &graph,
            "MATCH (a:person {name: 'marko'}) OPTIONAL MATCH (a)-[:knows]->(b {name: 'nobody'}) \
             WITH coalesce(b, a) AS x MATCH (x)-[:created]->(d) RETURN x.name AS x, d.name AS d",
            &["x", "d"]
        ),
        sorted(&["'marko'|'lop'"])
    );
    // The same rows as the node variable itself (rewrite pair).
    for (value, node) in [
        (
            "MATCH (p:person) WITH collect(p) AS ps FOR i IN range(0, size(ps) - 1) \
             WITH ps[i] AS x MATCH (x)-[:created]->(s) RETURN x.name AS x, s.name AS s",
            "MATCH (x:person)-[:created]->(s) RETURN x.name AS x, s.name AS s",
        ),
        (
            "MATCH (p:person)-[e:knows]->() WITH collect(e) AS es LET r = head(es) \
             MATCH (a)-[r]->(b) RETURN a.name AS x, b.name AS s",
            "MATCH (p:person)-[e:knows]->() WITH collect(e) AS es MATCH (a)-[r]->(b) \
             WHERE r = head(es) RETURN a.name AS x, b.name AS s",
        ),
    ] {
        assert_eq!(
            table(&graph, value, &["x", "s"]),
            table(&graph, node, &["x", "s"]),
            "{value}"
        );
    }
    // TCK Path1 [1] / Path2 [3]: NULL matches nothing, OPTIONAL MATCH keeps the row.
    assert_eq!(
        table(
            &graph,
            "WITH null AS a OPTIONAL MATCH p = (a)-[r]->() RETURN nodes(p) AS n, r",
            &["n", "r"]
        ),
        sorted(&["null|null"])
    );
    assert!(rows(&graph, "WITH null AS a MATCH (a)-->(b) RETURN b").is_empty());
}

#[test]
fn a_value_of_another_kind_as_a_pattern_element_is_a_data_error() {
    let graph = modern();
    // Known only when a row reaches the pattern: 22G03.
    for (query, message) in [
        (
            "MATCH (a:person) WITH collect(a.age) AS l LET x = l[0] MATCH (x)-->(b) RETURN b",
            "`x` is written as a node of the pattern but holds a value that is not a node \
             [22G03]",
        ),
        (
            "MATCH (a:person) WITH collect(a.age) AS l LET x = l[0] MATCH (y)-->(x) RETURN y",
            "`x` is written as a node of the pattern but holds a value that is not a node \
             [22G03]",
        ),
    ] {
        assert_eq!(failure(&graph, query), message, "{query}");
    }
    // An edge value is compared with the edges the pattern walks (no pattern starts at an
    // edge): another kind matches none.
    assert!(
        rows(
            &graph,
            "MATCH (a:person) WITH collect(a) AS l LET r = l[0] MATCH ()-[r]->(b) RETURN b"
        )
        .is_empty()
    );
    // A literal is certainly no element: refused before the run, as before; so is the value
    // column of a materializing WITH.
    assert_eq!(
        failure(
            &graph,
            "MATCH (a:person) WITH a.name AS n LIMIT 1 MATCH (n)-->(b) RETURN b"
        ),
        "the variable `n` is used as a node here but holds a value computed by WITH, not a \
         node or an edge [42002]"
    );
    assert_eq!(
        failure(&graph, "WITH 1 AS a MATCH (a)-->(b) RETURN b"),
        "`a` is a value defined by LET or WITH, not a node; a pattern variable is a new name or \
         a node or edge variable [42002]"
    );
    // A FOR item keeps its own rule (design E3): another kind gives no row.
    assert!(rows(&graph, "FOR x IN [1] MATCH (x)-->(b) RETURN b").is_empty());
}

#[test]
fn create_connects_nodes_held_by_values() {
    // TCK Match4 [4] (setup): the endpoints are items of a list.
    let graph = modern();
    let created = graph
        .write()
        .traversal_mut()
        .gql(
            "MATCH (a:person) WITH a ORDER BY a.name WITH collect(a) AS l \
             UNWIND range(0, size(l) - 2) AS i WITH l[i] AS n1, l[i + 1] AS n2 \
             CREATE (n1)-[:next]->(n2) RETURN count(*) AS c",
        )
        .to_list()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(created.len(), 1);
    assert_eq!(
        table(
            &graph,
            "MATCH (a)-[:next]->(b) RETURN a.name AS a, b.name AS b",
            &["a", "b"]
        ),
        sorted(&["'josh'|'marko'", "'marko'|'peter'", "'peter'|'vadas'"])
    );
    // A value that is no node fails when the edge is created.
    let message = match graph
        .write()
        .traversal_mut()
        .gql("WITH [1] AS l WITH l[0] AS x CREATE (x)-[:T]->(:Y)")
        .to_list()
    {
        Ok(rows) => panic!("{rows:?}"),
        Err(e) => match e.root_cause() {
            TraverserError::Gql { gqlstatus, .. } => gqlstatus.clone(),
            other => panic!("{other}"),
        },
    };
    assert_eq!(message, "22G03");
}

// ----- the WHERE of a WITH sees the names before it (B5) -----

#[test]
fn the_where_of_a_with_reads_the_variables_it_drops() {
    let graph = modern();
    // TCK WithWhere7 [1] / [3]: an item and a dropped variable side by side.
    for (query, rewrite) in [
        (
            "MATCH (a:person) WITH a.name AS name WHERE a.age > 30 RETURN name",
            "MATCH (a:person) WHERE a.age > 30 RETURN a.name AS name",
        ),
        (
            "MATCH (a:person) WITH a.name AS name WHERE name = 'josh' OR a.age < 28 RETURN name",
            "MATCH (a:person) WHERE a.name = 'josh' OR a.age < 28 RETURN a.name AS name",
        ),
        // TCK WithWhere1 [3] / TriadicSelection1: an OPTIONAL MATCH edge tested after WITH.
        (
            "MATCH (a:person) OPTIONAL MATCH (a)-[r:created]->() WITH a.name AS name \
             WHERE r IS NULL RETURN name",
            "MATCH (a:person) WHERE NOT EXISTS { (a)-[:created]->() } RETURN a.name AS name",
        ),
        // A pattern predicate on a dropped variable.
        (
            "MATCH (a)-->(b) WITH a.name AS name WHERE (b)-->() RETURN name",
            "MATCH (a)-->(b) WHERE (b)-->() RETURN a.name AS name",
        ),
    ] {
        assert_eq!(
            table(&graph, query, &["name"]),
            table(&graph, rewrite, &["name"]),
            "{query}"
        );
        assert!(!rows(&graph, query).is_empty(), "{query}");
    }
    // TCK WithWhere1 [2]: DISTINCT folds the rows the WHERE keeps.
    assert_eq!(
        table(
            &graph,
            "MATCH (a:person) WITH DISTINCT a.age > 30 AS old WHERE a.name <> 'josh' RETURN old",
            &["old"]
        ),
        sorted(&["false", "true"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (a:person) WITH DISTINCT a.age > 30 AS old WHERE a.name IN ['josh', 'peter'] \
             RETURN old",
            &["old"]
        ),
        sorted(&["true"])
    );
    // After ORDER BY / LIMIT the WHERE filters the rows kept (the three first names, then the
    // age test).
    assert_eq!(
        table(
            &graph,
            "MATCH (a:person) WITH a.name AS name ORDER BY name LIMIT 3 WHERE a.age > 30 \
             RETURN name",
            &["name"]
        ),
        sorted(&["'josh'", "'peter'"])
    );
    // A FOR item and a LET value dropped by the WITH.
    assert_eq!(
        table(
            &graph,
            "FOR x IN [1, 2, 3] WITH x * 2 AS y ORDER BY y LIMIT 2 WHERE x > 1 RETURN y",
            &["y"]
        ),
        sorted(&["4"])
    );
    // Later clauses do not see them; nor does the WHERE after an aggregation.
    assert_eq!(
        failure(
            &graph,
            "MATCH (a:person) WITH a.name AS name WHERE a.age > 30 RETURN a.age"
        ),
        "the variable `a` is not in scope: the WITH before does not carry it [42002]"
    );
    assert_eq!(
        failure(
            &graph,
            "MATCH (a:person) WITH a.name AS name, count(*) AS c WHERE a.age > 1 RETURN name"
        ),
        "the variable `a` is not in scope: the WITH before does not carry it [42002]"
    );
    assert_eq!(
        failure(
            &graph,
            "MATCH (a:person) WITH DISTINCT a.name AS name LIMIT 3 WHERE a.age > 28 RETURN name"
        ),
        "a WHERE after WITH DISTINCT and ORDER BY, OFFSET or LIMIT that reads a variable the \
         WITH drops is not yet supported in this version [0A000]"
    );
}

#[test]
fn a_with_where_reading_only_its_items_keeps_its_plan() {
    let graph = modern();
    // The filter of a projecting WITH reads the substituted item: the same steps as written
    // on the MATCH.
    assert_eq!(
        steps(
            &graph,
            "MATCH (a:person) WITH a.name AS name WHERE a.age > 30 RETURN name"
        ),
        steps(
            &graph,
            "MATCH (a:person) WITH a.name AS name, a.age AS age WHERE age > 30 RETURN name"
        )
    );
}
