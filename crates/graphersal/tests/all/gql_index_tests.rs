//! GQL list indexing and slicing (stage 3f, Cypher compatibility forms measured against the
//! openCypher TCK `List1`, `List2`, `Map2`, `Graph7`): `x[i]`, `x[a..b]`, `m[k]` and `n[k]`
//! run end to end through the ordinary evaluator steps (no new step). `n['key']` with a string
//! literal is the property access `n.key` (the same plan, pushdown included); a computed key
//! is read per row. Hand-computed rows, rewrite pairs (`x[i]` vs `head(x[i..i+1])`, a slice
//! vs a list comprehension over `range()`), a Gremlin twin, the errors and their phase,
//! writes reading a subscript, and the materialization limit of a slice.
#![cfg(feature = "gql")]

use std::sync::Arc;

use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn rows_with(
    graph: &Arc<Graph>,
    query: &str,
    params: &[(&str, ElementProperty)],
) -> Vec<ElementProperty> {
    graph
        .read()
        .traversal()
        .gql_with(query, params.iter().map(|(k, v)| (*k, v.clone())))
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

fn rows(graph: &Arc<Graph>, query: &str) -> Vec<ElementProperty> {
    rows_with(graph, query, &[])
}

/// The failure of `query`: the message of its GQL root cause and its GQLSTATUS.
fn failure(graph: &Arc<Graph>, query: &str, params: &[(&str, ElementProperty)]) -> String {
    let result = graph
        .read()
        .traversal()
        .gql_with(query, params.iter().map(|(k, v)| (*k, v.clone())))
        .to_list();
    match result {
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
fn steps(graph: &Arc<Graph>, query: &str, params: &[(&str, ElementProperty)]) -> Vec<String> {
    let profile = graph
        .read()
        .traversal()
        .gql_with(query, params.iter().map(|(k, v)| (*k, v.clone())))
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

/// A value as comparable text keeping its type: `null`, `'text'`, `i:1`, `f:1.0`, `true`,
/// `[..]`, `{k: v, ..}` (keys sorted), `v<id>`, `e<id>`.
fn norm(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => format!("'{s}'"),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) if n.is_i64() => format!("i:{i}"),
            (_, Some(f)) => format!("f:{f:?}"),
            _ => format!("{n:?}"),
        },
        ElementProperty::Array(items) => {
            let items: Vec<String> = items.iter().map(norm).collect();
            format!("[{}]", items.join(", "))
        }
        ElementProperty::Object(map) => {
            let mut items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", norm(v)))
                .collect();
            items.sort();
            format!("{{{}}}", items.join(", "))
        }
        ElementProperty::Vertex(v) => format!("v{}", v.id()),
        ElementProperty::Edge(e) => format!("e{}", e.id()),
        other => format!("{other:?}"),
    }
}

/// Every row of `query` as `a|b|..` over its columns `keys`, sorted (a multiset).
fn table_with(
    graph: &Arc<Graph>,
    query: &str,
    keys: &[&str],
    params: &[(&str, ElementProperty)],
) -> Vec<String> {
    let mut out: Vec<String> = rows_with(graph, query, params)
        .iter()
        .map(|row| match row {
            ElementProperty::Object(map) => keys
                .iter()
                .map(|k| norm(map.get(k).unwrap_or(&ElementProperty::Null)))
                .collect::<Vec<_>>()
                .join("|"),
            other => panic!("{query}: not a row map: {other:?}"),
        })
        .collect();
    out.sort();
    out
}

fn table(graph: &Arc<Graph>, query: &str, keys: &[&str]) -> Vec<String> {
    table_with(graph, query, keys, &[])
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

#[test]
fn indexing_and_slicing_read_lists_maps_and_elements() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "RETURN [1, 2, 3][0] AS a, [1, 2, 3][-1] AS b, [1, 2, 3][3] AS c, [[1]][0][0] AS d, \
             [1, 2, 3, 4, 5][1..3] AS e, [1, 2, 3][..2] AS f, [1, 2, 3][1..] AS g, \
             [1, 2, 3][3..1] AS h, [1, 2, 3][-5..5] AS i, [1, 2, 3][1..null] AS j",
            &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]
        ),
        ["i:1|i:3|null|i:1|[i:2, i:3]|[i:1, i:2]|[i:2, i:3]|[]|[i:1, i:2, i:3]|null"]
    );
    // Maps: a string key, case-sensitive, missing NULL.
    assert_eq!(
        table(
            &graph,
            "WITH {name: 'Mats', Name: 'Pontus', null: 'x'} AS m \
             RETURN m['name'] AS a, m['Name'] AS b, m['nAMe'] AS c, m['null'] AS d, \
             m['na' || 'me'] AS e, m[null] AS f",
            &["a", "b", "c", "d", "e", "f"]
        ),
        ["'Mats'|'Pontus'|null|'x'|'Mats'|null"]
    );
    // Elements: a computed key reads the property per row.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p['na' || 'me'] AS n, p['missing' || ''] AS m",
            &["n", "m"]
        ),
        sorted(&[
            "'josh'|null",
            "'marko'|null",
            "'peter'|null",
            "'vadas'|null"
        ])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (:person {name: 'marko'})-[e:knows]->(f) RETURN e['wei' || 'ght'] AS w, \
             f['name'] AS f",
            &["w", "f"]
        ),
        sorted(&["f:0.5|'vadas'", "f:1.0|'josh'"])
    );
    // Parameters as the list, the index and the bounds.
    let params = [
        ("xs", ElementProperty::from(vec![10i64, 20, 30])),
        ("i", ElementProperty::from(-2)),
        ("from", ElementProperty::from(1)),
        ("to", ElementProperty::from(3)),
        ("key", ElementProperty::from("age")),
    ];
    assert_eq!(
        table_with(
            &graph,
            "MATCH (p:person {name: 'josh'}) RETURN $xs[$i] AS a, $xs[$from..$to] AS b, \
             p[$key] AS c",
            &["a", "b", "c"],
            &params
        ),
        ["i:20|[i:20, i:30]|i:32"]
    );
}

#[test]
fn subscripts_compose_with_every_clause() {
    let graph = modern();
    // WHERE, ORDER BY, UNWIND, LET / WITH substitution, comprehensions, aggregates.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) WHERE [p.age, 0][0] > 30 RETURN p.name AS n",
            &["n"]
        ),
        sorted(&["'josh'", "'peter'"])
    );
    let ordered: Vec<String> = rows(
        &graph,
        "MATCH (p:person) RETURN p.name AS n ORDER BY [p.age][0] DESC",
    )
    .iter()
    .map(norm)
    .collect();
    assert_eq!(
        ordered,
        [
            "{n: 'peter'}",
            "{n: 'josh'}",
            "{n: 'marko'}",
            "{n: 'vadas'}"
        ]
    );
    assert_eq!(
        table(
            &graph,
            "WITH [[1, 2], [3]] AS xs UNWIND xs[0] AS x RETURN x",
            &["x"]
        ),
        sorted(&["i:1", "i:2"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person {name: 'marko'}) LET pair = [p.name, p.age] \
             WITH pair[1] AS age, pair RETURN age, pair[-2] AS name",
            &["age", "name"]
        ),
        ["i:29|'marko'"]
    );
    assert_eq!(
        table(
            &graph,
            "WITH [[1, 2], [3, 4, 5]] AS xs RETURN [x IN xs | x[-1]] AS lasts, \
             [x IN xs WHERE x[2] IS NOT NULL | x[1..]] AS tails",
            &["lasts", "tails"]
        ),
        ["[i:2, i:5]|[[i:4, i:5]]"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) WITH p ORDER BY p.age RETURN collect(p.name)[0] AS youngest, \
             size(collect(p.name)[1..]) AS rest, max([p.age][0]) AS oldest",
            &["youngest", "rest", "oldest"]
        ),
        ["'vadas'|i:3|i:35"]
    );
    // A pattern comprehension's list and a path's nodes.
    assert_eq!(
        table(
            &graph,
            "MATCH p = (:person {name: 'marko'})-[:created]->(s) \
             RETURN nodes(p)[-1].name AS last, [(a:person {name: 'marko'})-[:knows]->(b) \
             | b.name][0..5] AS friends",
            &["last", "friends"]
        )
        .len(),
        1
    );
}

#[test]
fn rewrite_pairs_agree() {
    let graph = modern();
    // `x[i]` is `head(x[i..i+1])` for every i in range and out of it, negatives included.
    for i in -4..=4 {
        let left = table(
            &graph,
            &format!("MATCH (p:person) RETURN [p.name, p.age, 7][{i}] AS x"),
            &["x"],
        );
        let to = if i == -1 {
            String::new()
        } else {
            format!("{}", i + 1)
        };
        let right = table(
            &graph,
            &format!("MATCH (p:person) RETURN head([p.name, p.age, 7][{i}..{to}]) AS x"),
            &["x"],
        );
        assert_eq!(left, right, "x[{i}]");
    }
    // `x[a..b]` with in-range non-negative bounds is the comprehension over the positions.
    for (a, b) in [(0, 0), (0, 2), (1, 3), (2, 3), (3, 3)] {
        let left = table(
            &graph,
            &format!("MATCH (p:person) RETURN [p.name, p.age, 7][{a}..{b}] AS x"),
            &["x"],
        );
        let right = table(
            &graph,
            &format!(
                "MATCH (p:person) WITH [p.name, p.age, 7] AS xs \
                 RETURN [i IN range(0, size(xs) - 1) WHERE i >= {a} AND i < {b} | xs[i]] AS x"
            ),
            &["x"],
        );
        assert_eq!(left, right, "x[{a}..{b}]");
    }
}

#[test]
fn a_string_literal_key_is_the_property_access_with_its_plan() {
    let graph = modern();
    for (subscript, property) in [
        (
            "MATCH (p:person) WHERE p['name'] = 'marko' RETURN p['age'] AS a",
            "MATCH (p:person) WHERE p.name = 'marko' RETURN p.age AS a",
        ),
        (
            "MATCH (p:person {name: 'josh'})-[:created]->(s) RETURN s['name'] AS n",
            "MATCH (p:person {name: 'josh'})-[:created]->(s) RETURN s.name AS n",
        ),
    ] {
        assert_eq!(
            table(&graph, subscript, &["a", "n"]),
            table(&graph, property, &["a", "n"])
        );
        let left: Vec<String> = steps(&graph, subscript, &[])
            .iter()
            .map(|s| s.replace("['name']", ".name").replace("['age']", ".age"))
            .collect();
        assert_eq!(left, steps(&graph, property, &[]), "{subscript}");
    }
    // The pushdown is there; a computed key is read per row (no pushdown).
    let pushed = steps(
        &graph,
        "MATCH (p:person) WHERE p['name'] = 'marko' RETURN p",
        &[],
    );
    assert!(
        pushed.iter().any(|s| s.starts_with("has(\"name\"")),
        "{pushed:?}"
    );
    let dynamic = steps(
        &graph,
        "MATCH (p:person) WHERE p[$k] = 'marko' RETURN p",
        &[("k", ElementProperty::from("name"))],
    );
    assert!(
        dynamic.iter().any(|s| s.starts_with("gql:filter")),
        "{dynamic:?}"
    );
    assert_eq!(
        table_with(
            &graph,
            "MATCH (p:person) WHERE p[$k] = 'marko' RETURN p.age AS a",
            &["a"],
            &[("k", ElementProperty::from("name"))]
        ),
        ["i:29"]
    );
}

#[test]
fn subscripts_of_the_wrong_kind_fail_when_a_row_reaches_them() {
    let graph = modern();
    for (query, expected) in [
        (
            "WITH [1, 2] AS xs RETURN xs[1.5]",
            "a list index is an integer, not a float [22G03]",
        ),
        (
            "WITH [1, 2] AS xs, '1' AS i RETURN xs[i]",
            "a list index is an integer, not a string [22G03]",
        ),
        (
            "WITH 'abc' AS s RETURN s[0]",
            "cannot index a string: [ ] reads an item of a list, an entry of a map or a \
             property of a node or an edge [22G03]",
        ),
        (
            "WITH {a: 1} AS m RETURN m[0]",
            "a map is read with a string key, not a number [22G03]",
        ),
        (
            "MATCH (p:person) RETURN p[1]",
            "a property of a node or an edge is read with a string key, not a number [22G03]",
        ),
        (
            "WITH [1, 2] AS xs RETURN xs[0.5..]",
            "a slice bound is an integer, not a float [22G03]",
        ),
        (
            "WITH 'abc' AS s RETURN s[0..1]",
            "a slice [a..b] reads a list, not a string [22G03]",
        ),
    ] {
        assert_eq!(failure(&graph, query, &[]), expected, "{query}");
    }
    // Parameters stay values: the error arises at run time, by the row (TCK Map2 [6]-[8]).
    assert_eq!(
        failure(
            &graph,
            "WITH $expr AS expr, $idx AS idx RETURN expr[idx]",
            &[
                ("expr", ElementProperty::from(100)),
                ("idx", ElementProperty::from(0))
            ]
        ),
        "cannot index a number: [ ] reads an item of a list, an entry of a map or a property \
         of a node or an edge [22G03]"
    );
    // No row reaches the subscript: no error.
    assert!(rows(&graph, "MATCH (n:nothing) RETURN 'abc'[n.x] AS x").is_empty());
}

#[test]
fn writes_read_subscripts() {
    // DELETE of a list item (TCK Delete5): the element kind is known at run time only.
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .gql(
            "MATCH (p:person) WITH p ORDER BY p.age WITH collect(p) AS people \
             DETACH DELETE people[0], people[-1]",
        )
        .iterate()
        .unwrap_or_else(|e| panic!("{e}"));
    let names: Vec<String> = graph
        .traversal()
        .gql("MATCH (p:person) RETURN p.name AS n ORDER BY n")
        .select("n")
        .to_list()
        .unwrap()
        .iter()
        .map(norm)
        .collect();
    assert_eq!(names, ["'josh'", "'marko'"]);
    // SET / CREATE values and MERGE properties from subscripts.
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .gql(
            "MATCH (p:person {name: 'marko'}) WITH p, ['a', 'b', 'c'] AS xs \
             SET p.first = xs[0], p.tail = xs[1..] \
             CREATE (:tag {name: xs[-1]}) MERGE (:tag {name: xs[1]})",
        )
        .iterate()
        .unwrap_or_else(|e| panic!("{e}"));
    let out: Vec<String> = graph
        .traversal()
        .gql(
            "MATCH (p:person {name: 'marko'}), (t:tag) RETURN p.first AS f, p.tail AS t, \
             t.name AS n ORDER BY n",
        )
        .to_list()
        .unwrap()
        .iter()
        .map(norm)
        .collect();
    assert_eq!(
        out,
        [
            "{f: 'a', n: 'b', t: ['b', 'c']}",
            "{f: 'a', n: 'c', t: ['b', 'c']}"
        ]
    );
    // A dynamic key as a SET target stays "not yet".
    let mut graph = TraversalGraph::tinkerpop_modern();
    let error = graph
        .traversal_mut()
        .gql("MATCH (n:person) SET n['k'] = 1")
        .iterate()
        .expect_err("not yet");
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::Gql {
                kind: GqlErrorKind::NotYet,
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn a_slice_of_a_constant_list_asks_for_its_size() {
    let graph = modern();
    let big: Vec<i64> = (0..100_000).collect();
    let lock = graph.read();
    let error = lock
        .traversal()
        .with("traversal.max_materialized_bytes", 100_000i64)
        .gql_with("RETURN $xs[1..] AS x", [("xs", ElementProperty::from(big))])
        .to_list()
        .expect_err("the slice is larger than the limit");
    match error.root_cause() {
        TraverserError::ResourceLimitExceeded { limit, .. } => {
            assert_eq!(limit, "traversal.max_materialized_bytes")
        }
        other => panic!("expected a resource limit, got {other}"),
    }
    // A small slice of the same list runs under the same limit.
    let big: Vec<i64> = (0..100_000).collect();
    let rows = lock
        .traversal()
        .with("traversal.max_materialized_bytes", 100_000i64)
        .gql_with(
            "RETURN $xs[-3..] AS x, $xs[99999] AS y",
            [("xs", ElementProperty::from(big))],
        )
        .to_list()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        rows.iter().map(norm).collect::<Vec<_>>(),
        ["{x: [i:99997, i:99998, i:99999], y: i:99999}"]
    );
}

#[cfg(feature = "script")]
mod twins {
    use super::{modern, norm};
    use graphersal::prelude::*;
    use std::sync::Arc;

    /// The values of the Rhai script `script` (a traversal), as sorted `norm` texts.
    fn values(graph: &Arc<Graph>, script: &str) -> Vec<String> {
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
        let lock = graph.read();
        let mut out: Vec<String> = gts
            .ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .iter()
            .map(norm)
            .collect();
        out.sort();
        out
    }

    #[test]
    fn subscripts_match_gremlin_values_and_select() {
        let graph = modern();
        for (gql, gremlin) in [
            (
                r#"g.gql("MATCH (p:person) RETURN p['na' || 'me'] AS x").select("x")"#,
                r#"g.V().hasLabel("person").values("name")"#,
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN [p.age, p.name][-1] AS x").select("x")"#,
                r#"g.V().hasLabel("person").values("name")"#,
            ),
            (
                r#"g.gql("MATCH (s:software) RETURN {n: s.name}['n'] AS x").select("x")"#,
                r#"g.V().hasLabel("software").values("name")"#,
            ),
        ] {
            let left = values(&graph, gql);
            assert!(!left.is_empty(), "{gql}");
            assert_eq!(
                left,
                values(&graph, gremlin),
                "\nGQL:  {gql}\ntwin: {gremlin}"
            );
        }
    }
}
