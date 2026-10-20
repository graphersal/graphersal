//! GQL stage 2d composite queries (feature `gql`): `UNION` / `UNION DISTINCT` / `UNION ALL` and
//! `NEXT` (design item 8, E15, E19). A `UNION` block lowers to `union(__.<operand>, ..)` (the
//! start step `g.union(..)` at the start of the query, a mid-traversal `union()` after `NEXT`),
//! each operand lowered like a query of its own (own pushdown and index), then `gql:distinct()`
//! for `UNION` / `UNION DISTINCT`. Differential twins against Gremlin `g.union(..)` (Rhai) with
//! `dedup()` where GQL equality agrees with it compare the rows as multisets (design D18);
//! GQL equality (`1 = 1.0`, `NULL`s equal), column reordering, per-operand `ORDER BY` / `LIMIT`
//! and `NEXT` chains are hand-computed rows.
//!
//! Stage 2d.T5: `[OPTIONAL] CALL { }` (design mechanism 6, E16) is `flat_map(__.<body>)`, the
//! body lowered onto the child from the incoming row; twins against Gremlin `flat_map(..)` and
//! `local(..)`, per-row aggregation and top-k, imports from labels / the current element / row
//! columns, `OPTIONAL CALL` `NULL`s, bulk and the stack.
//!
//! Stage 2d.T5c (user decision D23): whole-query subquery bodies (`EXISTS { .. RETURN .. }`,
//! `COUNT { }`, `COLLECT { .. RETURN x }`) lowered like a `CALL` body onto the subquery's child;
//! twins against Gremlin `local(__..count())` / `local(__..fold())` and the pattern
//! comprehension, per-row semantics, bulk, plans, list limits and the stack.
#![cfg(feature = "gql")]

use std::sync::Arc;

use graphersal::catalog::PropertyIndexDefinition;
use graphersal::prelude::*;
use graphersal::storage::ElementKind;

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

fn error(graph: &Arc<Graph>, query: &str) -> String {
    match graph.read().traversal().gql(query).to_list() {
        Ok(rows) => panic!("{query}: expected an error, got {rows:?}"),
        Err(e) => e.to_string(),
    }
}

fn profile(graph: &Arc<Graph>, query: &str) -> String {
    graph
        .read()
        .traversal()
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .to_string()
}

/// The steps of a profile, without the metrics columns (nested steps keep their `\>` marker).
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

fn norm(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => format!("'{s}'"),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => match n.as_i64() {
            Some(i) if n.is_i64() => format!("i:{i}"),
            _ => format!("{n:?}"),
        },
        ElementProperty::Vertex(v) => format!("v{}", v.id()),
        ElementProperty::Edge(e) => format!("e{}", e.id()),
        other => format!("{other:?}"),
    }
}

/// The column `key` of every row, in row order.
fn column(rows: &[ElementProperty], key: &str) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            ElementProperty::Object(map) => norm(
                map.get(key)
                    .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
            ),
            other => panic!("not a row map: {other:?}"),
        })
        .collect()
}

/// The column `key` of every row of `query`, in row order.
fn ordered(graph: &Arc<Graph>, query: &str, key: &str) -> Vec<String> {
    column(&rows(graph, query), key)
}

/// The column `key` of every row of `query`, sorted (a multiset).
fn multiset(graph: &Arc<Graph>, query: &str, key: &str) -> Vec<String> {
    let mut out = ordered(graph, query, key);
    out.sort();
    out
}

fn texts(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| format!("'{s}'")).collect()
}

fn ints(items: &[i64]) -> Vec<String> {
    items.iter().map(|i| format!("i:{i}")).collect()
}

// ----- differential twins against Gremlin `g.union(..)` (Rhai) -----

#[cfg(feature = "script")]
mod twins {
    use std::sync::Arc;

    use graphersal::prelude::*;

    use super::{modern, norm};

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

    /// The `x` column of the GQL query `gql` against the values of the Gremlin twin, as
    /// multisets.
    fn twin(graph: &Arc<Graph>, gql: &str, gremlin: &str) {
        let left = values(graph, &format!("g.gql({gql:?}).select(\"x\")"));
        let right = values(graph, gremlin);
        assert!(!left.is_empty(), "{gql}: no rows to compare");
        assert_eq!(left, right, "\nGQL:  {gql}\ntwin: {gremlin}");
    }

    #[test]
    fn union_all_is_gremlin_union() {
        let graph = modern();
        for (gql, gremlin) in [
            (
                "MATCH (p:person) RETURN p.name AS x UNION ALL MATCH (s:software) RETURN s.name AS x",
                r#"g.union(__.V().hasLabel("person").values("name"), __.V().hasLabel("software").values("name"))"#,
            ),
            // Duplicates stay: lop three times, josh twice.
            (
                "MATCH (:person)-[:created]->(s) RETURN s.name AS x \
                 UNION ALL MATCH (:person)-[:knows]->(f)-[:created]->(s) RETURN s.name AS x",
                r#"g.union(__.V().hasLabel("person").out("created").values("name"), __.V().hasLabel("person").out("knows").out("created").values("name"))"#,
            ),
            // Element columns stay live elements.
            (
                "MATCH (p:person)-[:created]->(s) RETURN s AS x UNION ALL MATCH (s:software) RETURN s AS x",
                r#"g.union(__.V().hasLabel("person").out("created"), __.V().hasLabel("software"))"#,
            ),
            // Same variable names in each operand are bindings of their own.
            (
                "MATCH (a:person)-[:knows]->(b) RETURN b.name AS x \
                 UNION ALL MATCH (a:software)<-[:created]-(b) RETURN b.name AS x",
                r#"g.union(__.V().hasLabel("person").out("knows").values("name"), __.V().hasLabel("software").in("created").values("name"))"#,
            ),
            // A unit operand (no MATCH) is `__.inject(0)` in its branch.
            (
                "RETURN 'none' AS x UNION ALL MATCH (s:software) RETURN s.name AS x",
                r#"g.union(__.inject("none"), __.V().hasLabel("software").values("name"))"#,
            ),
            // Three operands.
            (
                "MATCH (p:person) WHERE p.age > 30 RETURN p.name AS x \
                 UNION ALL MATCH (p:person) WHERE p.age < 30 RETURN p.name AS x \
                 UNION ALL MATCH (s:software) RETURN s.lang AS x",
                r#"g.union(__.V().hasLabel("person").has("age", P.gt(30)).values("name"), __.V().hasLabel("person").has("age", P.lt(30)).values("name"), __.V().hasLabel("software").values("lang"))"#,
            ),
        ] {
            twin(&graph, gql, gremlin);
        }
    }

    /// `UNION` / `UNION DISTINCT` is `dedup()` after the union where GQL equality agrees with
    /// Gremlin's (strings, elements).
    #[test]
    fn union_is_gremlin_union_then_dedup() {
        let graph = modern();
        for (gql, gremlin) in [
            (
                "MATCH (:person)-[:created]->(s) RETURN s.name AS x \
                 UNION MATCH (:person)-[:knows]->(f)-[:created]->(s) RETURN s.name AS x",
                r#"g.union(__.V().hasLabel("person").out("created").values("name"), __.V().hasLabel("person").out("knows").out("created").values("name")).dedup()"#,
            ),
            (
                "MATCH (p:person)-[:created]->(s) RETURN s AS x UNION DISTINCT MATCH (s:software) RETURN s AS x",
                r#"g.union(__.V().hasLabel("person").out("created"), __.V().hasLabel("software")).dedup()"#,
            ),
            // Duplicates inside one operand go too.
            (
                "MATCH (:person)-[:created]->(s) RETURN s.lang AS x UNION RETURN 'none' AS x",
                r#"g.union(__.V().hasLabel("person").out("created").values("lang"), __.inject("none")).dedup()"#,
            ),
        ] {
            twin(&graph, gql, gremlin);
        }
    }

    /// `NEXT` after a `UNION` continues from the records: a pattern from an element column.
    #[test]
    fn next_continues_a_pattern_from_the_union_records() {
        let graph = modern();
        twin(
            &graph,
            "MATCH (p:person) WHERE p.age > 30 RETURN p AS v UNION MATCH (s:software) RETURN s AS v \
             NEXT MATCH (v)<-[:created]-(c) RETURN c.name AS x",
            r#"g.union(__.V().hasLabel("person").has("age", P.gt(30)), __.V().hasLabel("software")).dedup().in("created").values("name")"#,
        );
        twin(
            &graph,
            "MATCH (p:person) RETURN p AS v UNION ALL MATCH (p:person) RETURN p AS v \
             NEXT MATCH (v)-[:knows]->(f) RETURN f.name AS x",
            r#"g.union(__.V().hasLabel("person"), __.V().hasLabel("person")).out("knows").values("name")"#,
        );
    }
}

// ----- UNION semantics: hand-computed rows -----

/// `UNION` removes duplicate records by GQL equality (decision C3): `1` and `1.0` are one
/// value (the first record is kept), `NULL`s are equal; `UNION ALL` keeps every record.
#[test]
fn union_removes_duplicates_by_gql_equality_and_union_all_keeps_them() {
    let graph = modern();
    let distinct = rows(
        &graph,
        "RETURN 1 AS x UNION RETURN 1.0 AS x UNION RETURN 2 AS x",
    );
    assert_eq!(column(&distinct, "x"), ints(&[1, 2]));
    let all = rows(
        &graph,
        "RETURN 1 AS x UNION ALL RETURN 1.0 AS x UNION ALL RETURN 2 AS x",
    );
    assert_eq!(all.len(), 3);
    assert_eq!(
        ordered(&graph, "RETURN null AS x UNION RETURN null AS x", "x"),
        ["null"]
    );
    assert_eq!(
        ordered(&graph, "RETURN null AS x UNION ALL RETURN null AS x", "x"),
        ["null", "null"]
    );
    // Whole records are compared: equal x with different y stay apart.
    let pairs = rows(
        &graph,
        "RETURN 1 AS x, 'a' AS y UNION RETURN 1 AS x, 'b' AS y UNION RETURN 1.0 AS x, 'a' AS y",
    );
    assert_eq!(column(&pairs, "y"), texts(&["a", "b"]));
}

#[test]
fn union_and_union_all_with_duplicates() {
    let graph = modern();
    let created = "MATCH (:person)-[:created]->(s) RETURN s.name AS n";
    let software = "MATCH (s:software) RETURN s.name AS n";
    assert_eq!(
        multiset(&graph, &format!("{created} UNION ALL {software}"), "n"),
        texts(&["lop", "lop", "lop", "lop", "ripple", "ripple"])
    );
    assert_eq!(
        multiset(&graph, &format!("{created} UNION {software}"), "n"),
        texts(&["lop", "ripple"])
    );
    assert_eq!(
        multiset(&graph, &format!("{created} UNION DISTINCT {software}"), "n"),
        texts(&["lop", "ripple"])
    );
}

/// Design E15: the operands return the same names in any order; every record has the first
/// operand's column order.
#[test]
fn later_operands_are_reordered_to_the_first_operands_columns() {
    let graph = modern();
    let result = rows(
        &graph,
        "MATCH (p:person {name: 'marko'}) RETURN p.name AS n, p.age AS a \
         UNION ALL MATCH (s:software {name: 'lop'}) RETURN s.lang AS a, s.name AS n",
    );
    assert_eq!(result.len(), 2);
    for row in &result {
        let ElementProperty::Object(map) = row else {
            panic!("not a row map: {row:?}");
        };
        let keys: Vec<&str> = map.keys().map(|k| k.as_ref()).collect();
        assert_eq!(keys, ["n", "a"], "{row:?}");
    }
    assert_eq!(column(&result, "n"), texts(&["marko", "lop"]));
    assert_eq!(
        column(&result, "a"),
        ["i:29".to_string(), "'java'".to_string()]
    );
}

/// Design E15: `ORDER BY` / `OFFSET` / `LIMIT` belong to their operand; the records arrive
/// operand by operand.
#[test]
fn order_by_and_limit_are_per_operand() {
    let graph = modern();
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN p.name AS n ORDER BY p.age DESC LIMIT 2 \
             UNION ALL MATCH (s:software) RETURN s.name AS n ORDER BY n DESC OFFSET 1",
            "n"
        ),
        texts(&["peter", "josh", "lop"])
    );
    // An aggregation per operand.
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN count(*) AS c UNION ALL MATCH (s:software) RETURN count(*) AS c",
            "c"
        ),
        ints(&[4, 2])
    );
    // An aggregating operand ordered by a subquery over a grouped element (the `ORDER BY` of
    // a later operand reads its own `RETURN` row label).
    let result = rows(
        &graph,
        "MATCH (s:software) RETURN s AS n, count(*) AS c UNION ALL \
         MATCH (p:person) RETURN p AS n, count(*) AS c \
         ORDER BY size([(p)-[:created]->(x) | x]) DESC, p.name",
    );
    let names: Vec<String> = column(&result, "n").into_iter().skip(2).collect();
    assert_eq!(names, ["v4", "v1", "v6", "v2"]);
}

/// Subqueries in operands are hosted like in a query of their own.
#[test]
fn operands_host_subqueries() {
    let graph = modern();
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name AS n \
             UNION ALL MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name AS n",
            "n"
        ),
        texts(&["josh", "marko", "peter", "vadas"])
    );
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person {name: 'josh'}) RETURN [(p)-[:created]->(s) | s.name] AS n \
             UNION ALL MATCH (p:person {name: 'vadas'}) RETURN [(p)-[:created]->(s) | s.name] AS n",
            "n"
        )
        .len(),
        2
    );
}

// ----- NEXT -----

/// Design E19: `NEXT` binds looser than `UNION`: `q1 UNION q2 NEXT RETURN x ORDER BY x` sorts
/// the whole union; an aggregation after `NEXT` reads every record.
#[test]
fn next_after_a_union_sorts_and_aggregates_the_whole_union() {
    let graph = modern();
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN p.name AS x UNION MATCH (s:software) RETURN s.name AS x \
             NEXT RETURN x ORDER BY x",
            "x"
        ),
        texts(&["josh", "lop", "marko", "peter", "ripple", "vadas"])
    );
    assert_eq!(
        ordered(
            &graph,
            "MATCH (:person)-[:created]->(s) RETURN s.name AS x UNION ALL \
             MATCH (s:software) RETURN s.name AS x NEXT RETURN count(*) AS c",
            "c"
        ),
        ints(&[6])
    );
    assert_eq!(
        ordered(
            &graph,
            "MATCH (:person)-[:created]->(s) RETURN s.name AS x UNION \
             MATCH (s:software) RETURN s.name AS x NEXT RETURN count(*) AS c",
            "c"
        ),
        ints(&[2])
    );
    // A FILTER, a LIMIT after the sort, and a third block.
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN p.name AS x UNION MATCH (s:software) RETURN s.name AS x \
             NEXT FILTER x <> 'lop' RETURN x ORDER BY x DESC LIMIT 3 \
             NEXT RETURN upper(x) AS y",
            "y"
        ),
        texts(&["VADAS", "RIPPLE", "PETER"])
    );
}

/// Each operand of a `UNION` block after `NEXT` reads every record of the block before (a
/// mid-traversal `union()`, whose children see the whole input), not one record at a time.
#[test]
fn every_operand_after_next_reads_the_whole_table() {
    let graph = modern();
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN p.age AS a \
             NEXT RETURN sum(a) AS x UNION ALL RETURN count(a) AS x UNION ALL RETURN max(a) AS x",
            "x"
        ),
        ints(&[123, 4, 35])
    );
    assert_eq!(
        ordered(
            &graph,
            "RETURN 1 AS x UNION ALL RETURN 2 AS x \
             NEXT RETURN x * 10 AS y UNION ALL RETURN x * 100 AS y \
             NEXT RETURN y ORDER BY y DESC",
            "y"
        ),
        ints(&[200, 100, 20, 10])
    );
    // The profile: the second union runs after the first one's records.
    let text = profile(
        &graph,
        "MATCH (p:person) RETURN p.age AS a UNION ALL MATCH (s:software) RETURN 0 AS a \
         NEXT RETURN sum(a) AS x UNION RETURN count(a) AS x",
    );
    let top: Vec<String> = steps(&text)
        .into_iter()
        .filter(|s| !s.starts_with("\\>"))
        .collect();
    assert_eq!(top.len(), 3, "{text}");
    assert!(
        top[0].starts_with("union(__.v(labels: [\"person\"])"),
        "{text}"
    );
    assert!(top[1].starts_with("union(__.gql:aggregate("), "{text}");
    assert_eq!(top[2], "gql:distinct()", "{text}");
}

/// `NEXT` after a linear query is the `WITH` it desugars to (design E19): the same plan.
#[test]
fn next_after_a_linear_query_plans_like_with() {
    let graph = modern();
    for (next, with) in [
        (
            "MATCH (p:person) RETURN p, p.age AS a NEXT FILTER a > 30 RETURN p.name AS n",
            "MATCH (p:person) WITH p, p.age AS a WHERE a > 30 RETURN p.name AS n",
        ),
        (
            "MATCH (p:person) RETURN p.name AS n ORDER BY n LIMIT 2 \
             NEXT MATCH (q:person {name: n}) RETURN q.age AS a",
            "MATCH (p:person) WITH p.name AS n ORDER BY n LIMIT 2 \
             MATCH (q:person {name: n}) RETURN q.age AS a",
        ),
    ] {
        assert_eq!(
            steps(&profile(&graph, next)),
            steps(&profile(&graph, with)),
            "\n{next}\n{with}"
        );
        assert_eq!(rows(&graph, next), rows(&graph, with), "{next}");
    }
}

// ----- plans -----

/// Each operand keeps its own pushdown and property index; `UNION` adds `gql:distinct()`
/// after the start step `union()`, `UNION ALL` does not.
#[test]
fn each_operand_keeps_its_index_and_pushdown() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .create_index(
            "person_age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        )
        .unwrap();
    let graph = Arc::new(Graph::new(graph));
    let query = "MATCH (p:person {age: 29}) RETURN p.name AS n \
                 UNION MATCH (q:person) WHERE q.age > 30 RETURN q.name AS n \
                 UNION MATCH (s:software) WHERE s.lang = 'java' RETURN s.lang AS n";
    let text = profile(&graph, query);
    let indexed = |text: &str| {
        steps(text)
            .iter()
            .filter(|s| *s == "\\> v(labels: [\"person\"]) [index: vertex person.age]")
            .count()
    };
    assert_eq!(indexed(&text), 2, "{text}");
    assert!(text.contains("property_index"), "{text}");
    let all = steps(&text);
    assert!(
        all[0].starts_with("union(__.v(labels: [\"person\"])"),
        "{text}"
    );
    assert!(
        all.iter().any(|s| s == "\\> v(labels: [\"software\"])"),
        "{text}"
    );
    assert!(
        all.iter().any(|s| s == "\\> has(\"lang\", P.eq(\"java\"))"),
        "{text}"
    );
    assert_eq!(
        all.last().map(String::as_str),
        Some("gql:distinct()"),
        "{text}"
    );
    assert!(!text.contains("dedup"), "{text}");
    assert_eq!(
        multiset(&graph, query, "n"),
        texts(&["java", "josh", "marko", "peter"])
    );

    let all = profile(&graph, &query.replace("UNION", "UNION ALL"));
    assert!(!all.contains("gql:distinct()"), "{all}");
    assert_eq!(indexed(&all), 2, "{all}");
}

/// A string equality is a value, never a path label: a variable named like the compared string
/// (`vadas`) does not change it, and inside every `UNION` operand the property index serves it
/// (before, `has(k, P.eq("s"))` read a same-named path label and the index served it only at
/// the root).
#[test]
fn a_string_equality_is_a_value_and_index_served_in_every_operand() {
    let graph = modern();
    assert_eq!(
        column(
            &rows(
                &graph,
                "MATCH (vadas:person)-[:knows]->(f {name: 'vadas'}) RETURN f.name AS n"
            ),
            "n"
        ),
        texts(&["vadas"])
    );
    assert_eq!(
        column(
            &rows(
                &graph,
                "MATCH (vadas:person)-[:knows]->(f) WHERE f.name = 'vadas' RETURN vadas.name AS n"
            ),
            "n"
        ),
        texts(&["marko"])
    );

    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .create_index(
            "person_name",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
        )
        .unwrap();
    let graph = Arc::new(Graph::new(graph));
    let query = "MATCH (p:person {name: 'marko'}) RETURN p.age AS n \
                 UNION MATCH (q:person) WHERE q.name = 'josh' RETURN q.age AS n";
    let text = profile(&graph, query);
    let indexed = steps(&text)
        .iter()
        .filter(|s| *s == "\\> v(labels: [\"person\"]) [index: vertex person.name]")
        .count();
    assert_eq!(indexed, 2, "{text}");
    assert_eq!(multiset(&graph, query, "n"), ints(&[29, 32]));
}

/// The first block starts with the start step `union()`, a unit operand with `inject(0)` in
/// its branch; no label is set that nothing reads.
#[test]
fn the_union_is_the_start_step() {
    let graph = modern();
    let text = profile(
        &graph,
        "RETURN 1 AS x UNION ALL MATCH (p:person) RETURN p.age AS x",
    );
    let all = steps(&text);
    assert!(all[0].starts_with("union(__.inject(0)"), "{text}");
    assert!(!text.contains("as("), "{text}");
    // NEXT reading an element column after a hop labels the rows once (`#n<k>`).
    let text = profile(
        &graph,
        "MATCH (p:person) RETURN p AS v UNION MATCH (s:software) RETURN s AS v \
         NEXT MATCH (v)-->(w) RETURN v.name AS a, w.name AS b",
    );
    assert_eq!(text.matches("as(\"#n").count(), 1, "{text}");
}

// ----- a CALL inside a UNION operand -----

/// A `CALL` inside a `UNION` operand runs in its branch (refused "not yet" before 2d.T5); the
/// column mismatch of the operands is still 42002.
#[test]
fn call_inside_a_union_operand_runs() {
    let graph = modern();
    assert_eq!(
        multiset(
            &graph,
            "RETURN 1 AS x UNION ALL CALL { RETURN 2 AS y } RETURN y AS x",
            "x"
        ),
        ints(&[1, 2])
    );
    let text = error(&graph, "RETURN 1 AS x UNION ALL RETURN 2 AS y");
    assert!(text.contains("42002"), "{text}");
}

// ----- CALL { } (stage 2d.T5) -----

/// `"a|b"` of the columns `a` and `b` of every row of `query`, sorted (a multiset).
fn pairs(graph: &Arc<Graph>, query: &str, a: &str, b: &str) -> Vec<String> {
    let result = rows(graph, query);
    let mut out: Vec<String> = column(&result, a)
        .into_iter()
        .zip(column(&result, b))
        .map(|(a, b)| format!("{a}|{b}"))
        .collect();
    out.sort();
    out
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

#[cfg(feature = "script")]
mod call_twins {
    use super::{modern, twins_values};

    /// The `x` column of the GQL query `gql` against each Gremlin twin (the `flat_map(..)` and
    /// the `local(..)` form), as multisets.
    fn twin(gql: &str, gremlins: &[&str]) {
        let graph = modern();
        let left = twins_values(&graph, &format!("g.gql({gql:?}).select(\"x\")"));
        assert!(!left.is_empty(), "{gql}: no rows to compare");
        for gremlin in gremlins {
            let right = twins_values(&graph, gremlin);
            assert_eq!(left, right, "\nGQL:  {gql}\ntwin: {gremlin}");
        }
    }

    /// An aggregation in the body runs per incoming row; a row whose body matches nothing still
    /// counts 0 (an aggregation without grouping keys gives one record).
    #[test]
    fn a_counting_body_is_flat_map_and_local_count() {
        twin(
            "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN count(f) AS c } RETURN c AS x",
            &[
                r#"g.V().hasLabel("person").flatMap(__.out().count())"#,
                r#"g.V().hasLabel("person").local(__.out().count())"#,
            ],
        );
        twin(
            "MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN sum(s.weight) AS w, \
             count(*) AS c } RETURN c AS x",
            &[r#"g.V().hasLabel("person").local(__.out("created").count())"#],
        );
    }

    /// Top-k per row: `ORDER BY` and `LIMIT` in the body run per incoming row.
    #[test]
    fn top_k_per_row_is_flat_map_and_local_order_limit() {
        twin(
            "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN f ORDER BY f.name DESC LIMIT 2 } \
             RETURN f.name AS x",
            &[
                r#"g.V().hasLabel("person").flatMap(__.out().order().by("name", Order.desc).limit(2)).values("name")"#,
                r#"g.V().hasLabel("person").local(__.out().order().by("name", Order.desc).limit(2)).values("name")"#,
            ],
        );
        twin(
            "MATCH (s:software) CALL (s) { MATCH (s)<-[:created]-(c) RETURN c.age AS x \
             ORDER BY x LIMIT 1 } RETURN x",
            &[
                r#"g.V().hasLabel("software").flatMap(__.in("created").values("age").order().limit(1))"#,
                r#"g.V().hasLabel("software").local(__.in("created").values("age").order().limit(1))"#,
            ],
        );
    }

    /// Imports read where they are: the current element, a path label after the outer pattern
    /// moved on, a column of a `WITH` row; a body without imports (E16) runs per row too.
    #[test]
    fn imports_from_the_current_element_labels_and_columns() {
        // The current element.
        twin(
            "MATCH (p:person) CALL (p) { MATCH (p)-[:knows]->(f) RETURN f.name AS x } RETURN x",
            &[r#"g.V().hasLabel("person").flatMap(__.out("knows").values("name"))"#],
        );
        // A label: the outer pattern stands on `s`.
        twin(
            "MATCH (p:person)-[:created]->(s) CALL (p) { MATCH (p)-[:knows]->(f) RETURN f.name AS x } \
             RETURN x",
            &[
                r#"g.V().hasLabel("person").as("p").out("created").flatMap(__.select("p").out("knows").values("name"))"#,
            ],
        );
        // Both, joined in the body.
        twin(
            "MATCH (p:person)-[:created]->(s) CALL (p, s) { MATCH (s)<-[:created]-(o) WHERE o <> p \
             RETURN o.name AS x } RETURN x",
            &[
                r#"g.V().hasLabel("person").as("p").out("created").flatMap(__.in("created").where(P.neq("p")).values("name"))"#,
            ],
        );
        // A column of a materializing WITH row.
        twin(
            "MATCH (p:person) WITH p ORDER BY p.age LIMIT 2 CALL (p) { MATCH (p)-[:knows]->(f) \
             RETURN f.name AS x } RETURN x",
            &[
                r#"g.V().hasLabel("person").order().by("age").limit(2).flatMap(__.out("knows").values("name"))"#,
            ],
        );
        // Uncorrelated (E16): per incoming row.
        twin(
            "MATCH (p:person) CALL { MATCH (s:software) RETURN s.name AS x } RETURN x",
            &[r#"g.V().hasLabel("person").flatMap(__.V().hasLabel("software").values("name"))"#],
        );
    }

    /// A `UNION` in the body is a mid-traversal `union()` per incoming row (`UNION` removes the
    /// duplicates of that row only); nested `CALL` and `NEXT` in a body.
    #[test]
    fn union_next_and_nested_calls_in_a_body() {
        twin(
            "MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN s.lang AS x \
             UNION MATCH (p)-[:knows]->(f)-[:created]->(s) RETURN s.lang AS x } RETURN x",
            &[
                r#"g.V().hasLabel("person").flatMap(__.union(__.out("created").values("lang"), __.out("knows").out("created").values("lang")).dedup())"#,
            ],
        );
        twin(
            "MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN s.lang AS x \
             UNION ALL MATCH (p)-[:knows]->(f)-[:created]->(s) RETURN s.lang AS x } RETURN x",
            &[
                r#"g.V().hasLabel("person").flatMap(__.union(__.out("created").values("lang"), __.out("knows").out("created").values("lang")))"#,
            ],
        );
        twin(
            "MATCH (p:person) CALL (p) { CALL (p) { MATCH (p)-->(f) RETURN f } RETURN count(f) AS x } \
             RETURN x",
            &[r#"g.V().hasLabel("person").local(__.out().count())"#],
        );
        twin(
            "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN f.name AS n NEXT RETURN count(n) AS x } \
             RETURN x",
            &[r#"g.V().hasLabel("person").local(__.out().values("name").count())"#],
        );
    }
}

/// The values of a Rhai script, for the `CALL` twins (the `twins` module's helper).
#[cfg(feature = "script")]
fn twins_values(graph: &Arc<Graph>, script: &str) -> Vec<String> {
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

/// Per-row aggregation keeps every incoming row (count 0 included) and pairs it with its own
/// result; top-k per group keeps the group's own top rows.
#[test]
fn call_aggregates_and_ranks_per_incoming_row() {
    let graph = modern();
    assert_eq!(
        pairs(
            &graph,
            "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN count(f) AS c } RETURN p.name AS n, c",
            "n",
            "c"
        ),
        sorted(&["'josh'|i:2", "'marko'|i:3", "'peter'|i:1", "'vadas'|i:0"])
    );
    // Top-k per group: the two oldest people who created each software.
    assert_eq!(
        pairs(
            &graph,
            "MATCH (s:software) CALL (s) { MATCH (s)<-[:created]-(c) RETURN c \
             ORDER BY c.age DESC LIMIT 2 } RETURN s.name AS s, c.name AS c",
            "s",
            "c"
        ),
        sorted(&["'lop'|'peter'", "'lop'|'josh'", "'ripple'|'josh'"])
    );
    // DISTINCT and an aggregation in the body are per row.
    assert_eq!(
        pairs(
            &graph,
            "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN DISTINCT f.lang AS l } \
             RETURN p.name AS n, l",
            "n",
            "l"
        ),
        sorted(&[
            "'marko'|null",
            "'marko'|'java'",
            "'josh'|'java'",
            "'peter'|'java'"
        ])
    );
    assert_eq!(
        pairs(
            &graph,
            "MATCH (p:person) CALL { WITH p MATCH (p)-->(f) RETURN count(*) AS c } \
             RETURN p.name AS n, c ORDER BY c",
            "n",
            "c"
        ),
        sorted(&["'josh'|i:2", "'marko'|i:3", "'peter'|i:1", "'vadas'|i:0"])
    );
    // Every incoming row runs the body, duplicates too.
    assert_eq!(
        pairs(
            &graph,
            "UNWIND [1, 2, 2] AS i CALL (i) { RETURN i * 10 AS j } RETURN i, j",
            "i",
            "j"
        ),
        sorted(&["i:1|i:10", "i:2|i:20", "i:2|i:20"])
    );
}

/// An empty body result drops the row; `OPTIONAL CALL` keeps it with the returned names `NULL`,
/// also when a later stage reads them after leaving the row.
#[test]
fn optional_call_keeps_rows_without_records_with_nulls() {
    let graph = modern();
    let body = "(p) { MATCH (p)-[:created]->(s) RETURN s.name AS sn }";
    assert_eq!(
        pairs(
            &graph,
            &format!("MATCH (p:person) CALL {body} RETURN p.name AS n, sn"),
            "n",
            "sn"
        ),
        sorted(&[
            "'marko'|'lop'",
            "'josh'|'ripple'",
            "'josh'|'lop'",
            "'peter'|'lop'"
        ])
    );
    assert_eq!(
        pairs(
            &graph,
            &format!("MATCH (p:person) OPTIONAL CALL {body} RETURN p.name AS n, sn"),
            "n",
            "sn"
        ),
        sorted(&[
            "'marko'|'lop'",
            "'vadas'|null",
            "'josh'|'ripple'",
            "'josh'|'lop'",
            "'peter'|'lop'"
        ])
    );
    // An element column read after a later MATCH left the row.
    assert_eq!(
        pairs(
            &graph,
            "MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:created]->(s) RETURN s } \
             MATCH (q:person) WHERE q.age > 34 RETURN p.name AS n, s.name AS sn",
            "n",
            "sn"
        ),
        sorted(&[
            "'marko'|'lop'",
            "'vadas'|null",
            "'josh'|'ripple'",
            "'josh'|'lop'",
            "'peter'|'lop'"
        ])
    );
    // A body that never matches: every row kept with NULL.
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:nothing]->(s) RETURN s.name AS sn } \
             RETURN sn",
            "sn"
        ),
        ["null", "null", "null", "null"]
    );
}

/// `CALL` after `OPTIONAL MATCH` (an absent import reads `NULL` in the body), a pattern
/// continuing from an element column the body returns, a `CALL` starting the query, and an
/// outer `WITH` column read in the body after it moved on.
#[test]
fn call_flows_with_optional_match_patterns_and_rows() {
    let graph = modern();
    assert_eq!(
        pairs(
            &graph,
            "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) CALL (p, s) { RETURN s.name AS sn } \
             RETURN p.name AS n, sn",
            "n",
            "sn"
        ),
        sorted(&[
            "'marko'|'lop'",
            "'vadas'|null",
            "'josh'|'ripple'",
            "'josh'|'lop'",
            "'peter'|'lop'"
        ])
    );
    // A pattern continues from the returned element (`gql:bind`).
    assert_eq!(
        pairs(
            &graph,
            "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN f } MATCH (f)-->(g) \
             RETURN f.name AS f, g.name AS g",
            "f",
            "g"
        ),
        sorted(&["'josh'|'ripple'", "'josh'|'lop'"])
    );
    // A query starting with CALL starts from the unit row.
    assert_eq!(
        multiset(
            &graph,
            "CALL { MATCH (s:software) RETURN s.name AS sn } RETURN sn",
            "sn"
        ),
        texts(&["lop", "ripple"])
    );
    // A WITH column read in the body after it walked away from the row.
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person) WITH p.name AS n, p CALL (p, n) { MATCH (p)-[:created]->(f) \
             RETURN n || '>' || f.name AS x } RETURN x",
            "x"
        ),
        texts(&["josh>lop", "josh>ripple", "marko>lop", "peter>lop"])
    );
    // A body reading nothing of the row: one record per incoming row.
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) CALL (p) { RETURN 1 AS one } RETURN count(*) AS c",
            "c"
        ),
        ints(&[4])
    );
}

/// Bulk: merged incoming rows (a body run once per merged traverser, its records multiplied by
/// the bulk) give the same rows as the unmerged run, for aggregation, top-k and `UNION` bodies.
#[test]
fn call_keeps_bulk_semantics() {
    let graph = modern();
    for query in [
        "MATCH ()<-[:knows]-()-[:created]->(s) CALL (s) { MATCH (s)<-[:created]-(c) RETURN count(c) AS x } \
         RETURN s.name AS n, x",
        "MATCH ()<-[:knows]-()-[:created]->(s) CALL (s) { MATCH (s)<-[:created]-(c) RETURN c.name AS x \
         ORDER BY x LIMIT 1 } RETURN s.name AS n, x",
        "MATCH ()<-[:knows]-()-[:created]->(s) CALL (s) { MATCH (s)<-[:created]-(c) RETURN c.name AS x \
         UNION RETURN s.lang AS x } RETURN s.name AS n, x",
        "MATCH ()<-[:knows]-()-[:created]->(s) OPTIONAL CALL (s) { MATCH (s)-->(c) RETURN c.name AS x } \
         RETURN s.name AS n, x",
        "MATCH ()<-[:knows]-()-[:created]->(s) CALL { MATCH (p:person) RETURN p.age AS x } RETURN s.name AS n, x",
    ] {
        let run = |merge: bool| {
            let result = graph
                .read()
                .traversal()
                .with("bulk.merge", merge)
                .gql(query)
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {e}"));
            let mut out: Vec<String> = column(&result, "n")
                .into_iter()
                .zip(column(&result, "x"))
                .map(|(n, x)| format!("{n}|{x}"))
                .collect();
            out.sort();
            out
        };
        let merged = run(true);
        assert!(!merged.is_empty(), "{query}");
        assert_eq!(merged, run(false), "{query}");
    }
    // The prefix merges marko's two `knows` rows into one traverser of bulk 2 before the CALL.
    let text = graph
        .read()
        .traversal()
        .with("bulk.merge", true)
        .gql("MATCH ()<-[:knows]-()-[:created]->(s) CALL (s) { RETURN 1 AS x } RETURN s.name AS n, x")
        .profile()
        .unwrap()
        .to_string();
    let flat_map = text
        .lines()
        .find(|line| line.starts_with("flat_map("))
        .unwrap_or_else(|| panic!("{text}"));
    let counts: Vec<&str> = flat_map
        .split("]")
        .last()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    // Call, In (traversers), Out (traversers), Bulk out.
    assert_eq!(&counts[..4], ["1", "1", "1", "2"], "{text}");
}

/// The plan: `flat_map(__.<body>)` with the body's steps as profile children, `as("#c<k>")`
/// only when a column is read after leaving the row, `OPTIONAL CALL` as
/// `optional(__.flat_map(..).as("#c<k>"))`, the body's root scan keeps the source pushdown.
#[test]
fn call_is_flat_map_of_the_body() {
    let graph = modern();
    let text = profile(
        &graph,
        "MATCH (p:person) CALL (p) { MATCH (p)-[:knows]->(f) RETURN f.name AS x } RETURN p.name AS n, x",
    );
    assert_eq!(
        steps(&text),
        [
            r#"v(labels: ["person"])"#,
            r#"as("p") [path: labels(p)]"#,
            r#"flat_map(__.out("knows").gql:project("x")) [path: labels(p)]"#,
            r#"\> out("knows")"#,
            r#"\> gql:project("x")"#,
            r#"gql:project("n", "x")"#,
        ],
        "{text}"
    );
    // A column read after a later stage left the row: the row label.
    let text = profile(
        &graph,
        "MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN f } MATCH (f)-->(g) \
         RETURN f.name AS a, g.name AS b",
    );
    assert_eq!(text.matches("as(\"#c").count(), 1, "{text}");
    assert!(text.contains("gql:bind(\"f\")"), "{text}");
    // OPTIONAL CALL: the row label inside the optional part.
    let text = profile(
        &graph,
        "MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:created]->(s) RETURN s.name AS m } \
         RETURN p.name AS n, m",
    );
    let all = steps(&text);
    assert!(
        all.iter()
            .any(|s| s.starts_with("optional(__.flat_map(") && s.contains(".as(\"#c")),
        "{text}"
    );
    // An uncorrelated body keeps its source pushdown inside the child.
    let text = profile(
        &graph,
        "MATCH (p:person) CALL { MATCH (s:software) RETURN s.name AS x } RETURN x",
    );
    assert!(
        steps(&text).contains(&r#"\> v(labels: ["software"])"#.to_string()),
        "{text}"
    );
}

/// The body's first component is served by the property index through `gql:lookup` when its
/// equality reads an import (design M3, E5), and a constant equality keeps the child-leading
/// indexed scan.
#[test]
fn a_call_body_uses_the_property_index() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .create_index(
            "person_age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        )
        .unwrap();
    let graph = Arc::new(Graph::new(graph));
    let query = "MATCH (p:person) CALL (p) { MATCH (q:person) WHERE q.age = p.age + 3 \
                 RETURN q.name AS x } RETURN p.name AS n, x";
    let text = profile(&graph, query);
    assert!(text.contains("gql:lookup("), "{text}");
    assert!(text.contains("[index: vertex person.age]"), "{text}");
    assert_eq!(
        pairs(&graph, query, "n", "x"),
        sorted(&["'marko'|'josh'", "'josh'|'peter'"])
    );
    let query = "MATCH (s:software) CALL { MATCH (q:person) WHERE q.age = 32 RETURN q.name AS x } \
                 RETURN x";
    let text = profile(&graph, query);
    assert!(
        steps(&text)
            .iter()
            .any(|s| s.starts_with(r#"\> v(labels: ["person"])"#) && s.contains("[index:")),
        "{text}"
    );
    assert_eq!(multiset(&graph, query, "x"), texts(&["josh", "josh"]));
}

/// Stack headroom (2d P0 rule): the deepest accepted `CALL` nesting (`bind::MAX_BODY_DEPTH`,
/// 16), each level a `UNION ALL` body or a correlated body importing the row, the tallest
/// expression innermost, parses, binds, plans, lowers and runs on the default test thread
/// (2 MiB; measured minimum 1.76 MB debug, see `ai/gql/reports-2d/T5.md`).
#[test]
fn the_deepest_calls_run_within_the_stack() {
    let graph = modern();
    let ors = (0..500)
        .map(|i| format!("x = {i}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut query = format!("WITH 1 AS x RETURN {ors} AS y");
    for _ in 0..16 {
        query = format!("CALL {{ {query} UNION ALL RETURN 2 AS y }} RETURN y");
    }
    assert_eq!(rows(&graph, &query).len(), 17);
    // Correlated levels, each importing the row and matching from it.
    let mut query = format!("MATCH (p:person) WITH p, 1 AS x RETURN {ors} AS y");
    for _ in 0..16 {
        query = format!("MATCH (p:person) CALL (p) {{ {query} }} RETURN y");
    }
    assert_eq!(rows(&graph, &query).len(), 4);
}

/// Stack headroom (2d P0 rule), stage 2d.T5c: the deepest accepted whole-query subquery
/// nesting (`bind::MAX_BODY_DEPTH`, 16 levels of `EXISTS` / `COUNT` / `COLLECT` bodies), the
/// tallest expression innermost, parses, binds, plans, lowers and runs on the default test
/// thread (2 MiB; measured minimum in `ai/gql/reports-2d/T5c.md`).
#[test]
fn the_deepest_whole_query_bodies_run_within_the_stack() {
    let graph = modern();
    let ors = |n: usize| {
        (0..n)
            .map(|i| format!("x = {i}"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let mut bodies = format!("EXISTS {{ LET x = 1 FILTER {} RETURN x }}", ors(440));
    for _ in 1..16 {
        bodies = format!("EXISTS {{ MATCH (n) WITH n WHERE {bodies} RETURN n }}");
    }
    let query = format!("MATCH (n) WHERE {bodies} RETURN n.name AS n");
    assert_eq!(rows(&graph, &query).len(), 6);
    let mut bodies = format!("COUNT {{ LET x = 1 FILTER {} RETURN x }}", ors(440));
    for _ in 1..16 {
        bodies = format!("size(COLLECT {{ MATCH (n) WITH n WHERE {bodies} > 0 RETURN n.name }})");
    }
    let query = format!("MATCH (n) RETURN {bodies} AS c");
    // Every level imports the outer `n`: one row each.
    assert_eq!(column(&rows(&graph, &query), "c"), ints(&[1; 6]));
}

// ----- stage 2d.T5c: COUNT { }, COLLECT { } and whole-query subquery bodies -----

/// The columns of every row as `name=value` texts in written key order (lists as their
/// `norm`ed items), rows sorted (a multiset) unless `ordered`.
fn table(graph: &Arc<Graph>, query: &str, ordered: bool) -> Vec<String> {
    fn text(value: &ElementProperty) -> String {
        match value {
            ElementProperty::Array(items) => format!(
                "[{}]",
                items.iter().map(text).collect::<Vec<_>>().join(", ")
            ),
            other => norm(other),
        }
    }
    let mut out: Vec<String> = rows(graph, query)
        .iter()
        .map(|row| match row {
            ElementProperty::Object(map) => map
                .iter()
                .map(|(k, v)| format!("{k}={}", text(v)))
                .collect::<Vec<_>>()
                .join(" "),
            other => panic!("{query}: not a row map: {other:?}"),
        })
        .collect();
    if !ordered {
        out.sort();
    }
    out
}

/// Twins of the whole-query bodies (user decision D23) against Gremlin `local(..)`: `COUNT`
/// of a body's rows is `local(__..count())`, `COLLECT { .. RETURN x }` is
/// `local(__..fold())` (an `ORDER BY` in the body against `order()`, so the lists compare).
#[cfg(feature = "script")]
mod body_twins {
    use super::{modern, twins_values};

    fn twin(gql: &str, gremlin: &str) {
        let graph = modern();
        let left = twins_values(&graph, &format!("g.gql({gql:?}).select(\"x\")"));
        assert!(!left.is_empty(), "{gql}: no rows to compare");
        assert_eq!(
            left,
            twins_values(&graph, gremlin),
            "\nGQL:  {gql}\ntwin: {gremlin}"
        );
    }

    #[test]
    fn count_of_a_whole_query_body_is_local_count() {
        for (gql, gremlin) in [
            (
                "MATCH (p:person) RETURN COUNT { MATCH (p)-->(f) WITH f WHERE f.age > 26 \
                 RETURN f } AS x",
                r#"g.V().hasLabel("person").local(__.out().has("age", P.gt(26)).count())"#,
            ),
            (
                "MATCH (p:person) RETURN COUNT { MATCH (p)-->()-->(g) RETURN DISTINCT g } AS x",
                r#"g.V().hasLabel("person").local(__.out().out().dedup().count())"#,
            ),
            (
                "MATCH (p:person) RETURN COUNT { MATCH (p)-[:knows]->(f) RETURN f AS y UNION \
                 MATCH (p)-[:created]->(f) RETURN f AS y } AS x",
                r#"g.V().hasLabel("person").local(__.union(__.out("knows"), __.out("created")).dedup().count())"#,
            ),
            (
                "MATCH (p:person) RETURN COUNT { MATCH (p)-->(f) RETURN f ORDER BY f.name \
                 LIMIT 2 } AS x",
                r#"g.V().hasLabel("person").local(__.out().limit(2).count())"#,
            ),
        ] {
            twin(gql, gremlin);
        }
    }

    #[test]
    fn collect_is_local_fold() {
        for (gql, gremlin) in [
            (
                "MATCH (p:person) RETURN COLLECT { MATCH (p)-->(f) RETURN f.name ORDER BY \
                 f.name } AS x",
                r#"g.V().hasLabel("person").local(__.out().values("name").order().fold())"#,
            ),
            (
                "MATCH (s:software) RETURN COLLECT { MATCH (s)<-[:created]-(c) RETURN c.age \
                 ORDER BY c.age DESC } AS x",
                r#"g.V().hasLabel("software").local(__.in("created").values("age").order().by(Order.desc).fold())"#,
            ),
            (
                "MATCH (p:person) RETURN COLLECT { MATCH (p)-[:created]->(s) RETURN s } AS x",
                r#"g.V().hasLabel("person").local(__.out("created").fold())"#,
            ),
        ] {
            twin(gql, gremlin);
        }
    }
}

/// `COLLECT { .. RETURN x }` gives the list of a pattern comprehension `[pattern | x]` (same
/// plan order) and the multiset of `OPTIONAL MATCH .. collect_list()`; `NULL` values stay in
/// the list; a `NULL` import gives `[]`.
#[test]
fn collect_is_the_pattern_comprehension_and_keeps_nulls() {
    let graph = modern();
    for (subquery, comprehension) in [
        (
            "MATCH (p:person) RETURN p.name AS n, COLLECT { MATCH (p)-[:knows]->(f) RETURN \
             f.name } AS l",
            "MATCH (p:person) RETURN p.name AS n, [(p)-[:knows]->(f) | f.name] AS l",
        ),
        (
            "MATCH (p:person) RETURN p.name AS n, COLLECT { MATCH (p)-->(f) WHERE f.age > 0 \
             RETURN f.age * 2 } AS l",
            "MATCH (p:person) RETURN p.name AS n, [(p)-->(f) WHERE f.age > 0 | f.age * 2] AS l",
        ),
        (
            "MATCH (p:person) RETURN p.name AS n, COLLECT { MATCH (p)-->(f) RETURN f.lang } AS l",
            "MATCH (p:person) RETURN p.name AS n, [(p)-->(f) | f.lang] AS l",
        ),
    ] {
        assert_eq!(
            table(&graph, subquery, false),
            table(&graph, comprehension, false),
            "{subquery}"
        );
    }
    // marko's out-neighbours: two persons (no lang) and lop.
    let marko = table(
        &graph,
        "MATCH (p:person {name: 'marko'}) RETURN COLLECT { MATCH (p)-->(f) RETURN f.lang } AS l",
        false,
    );
    assert_eq!(marko.len(), 1);
    assert_eq!(marko[0].matches("null").count(), 2, "{marko:?}");
    assert!(marko[0].contains("'java'"), "{marko:?}");
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WHERE s.lang = 'none' \
             RETURN p.name AS n, COLLECT { MATCH (s)<--(c) RETURN c.name } AS l",
            false
        ),
        strings(&[
            "n='josh' l=[]",
            "n='marko' l=[]",
            "n='peter' l=[]",
            "n='vadas' l=[]"
        ])
    );
}

fn strings(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

/// Whole-query bodies per row: `WITH` / `WHERE`, aggregation (one row even for no match, so
/// `EXISTS` of it is TRUE and `COUNT` of it is 1, as in Neo4j), `ORDER BY` / `LIMIT`,
/// `DISTINCT`, `UNION`, `NEXT`, `CALL` and nested subqueries inside, uncorrelated bodies, a
/// query starting with one.
#[test]
fn whole_query_bodies_run_per_row() {
    let graph = modern();
    // EXISTS with clauses other than MATCH (the fast path and the hook).
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WITH s WHERE s.lang = \
             'java' RETURN s } RETURN p.name AS n",
            "n"
        ),
        texts(&["josh", "marko", "peter"])
    );
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person) WHERE NOT EXISTS { MATCH (p)-->(f) WITH f ORDER BY f.name LIMIT 1 \
             RETURN f } RETURN p.name AS n",
            "n"
        ),
        texts(&["vadas"])
    );
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person) WHERE p.age > 100 OR EXISTS { MATCH (p)-[:created]->(s) RETURN s \
             UNION MATCH (p)<-[:knows]-(k) RETURN k AS s } RETURN p.name AS n",
            "n"
        ),
        texts(&["josh", "marko", "peter", "vadas"])
    );
    // An aggregating body gives a row for every outer row.
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) RETURN count(s) AS c } \
             RETURN p.name AS n",
            "n"
        ),
        texts(&["josh", "marko", "peter", "vadas"])
    );
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WITH count(s) AS c \
             WHERE c > 1 RETURN c } RETURN p.name AS n",
            "n"
        ),
        texts(&["josh"])
    );
    // COUNT of the body's rows.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, COUNT { MATCH (p)-[:created]->(s) RETURN \
             count(*) AS k } AS a, COUNT { MATCH (p)-->(f) RETURN f.name AS x, f.age AS y } AS b",
            false
        ),
        strings(&[
            "n='josh' a=i:1 b=i:2",
            "n='marko' a=i:1 b=i:3",
            "n='peter' a=i:1 b=i:1",
            "n='vadas' a=i:1 b=i:0"
        ])
    );
    // marko: lop's language, and the names of vadas and josh (UNION removes nothing here).
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, COUNT { MATCH (p)-[:created]->(s) RETURN \
             s.lang AS l UNION MATCH (p)-[:knows]->(s) RETURN s.name AS l } AS c",
            false
        ),
        strings(&[
            "n='josh' c=i:1",
            "n='marko' c=i:3",
            "n='peter' c=i:1",
            "n='vadas' c=i:0"
        ])
    );
    // COLLECT: ORDER BY / LIMIT, DISTINCT, NEXT, CALL, a nested COUNT, an aggregate.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, COLLECT { MATCH (p)-->(f) RETURN f.name ORDER \
             BY f.name DESC LIMIT 2 } AS l",
            false
        ),
        strings(&[
            "n='josh' l=['ripple', 'lop']",
            "n='marko' l=['vadas', 'lop']",
            "n='peter' l=['lop']",
            "n='vadas' l=[]"
        ])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (s:software) RETURN s.name AS n, COLLECT { MATCH (s)<-[:created]-(c) RETURN \
             DISTINCT c.name IS NOT NULL } AS l",
            false
        ),
        strings(&["n='lop' l=[true]", "n='ripple' l=[true]"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, COLLECT { MATCH (p)-->(f) RETURN f NEXT MATCH \
             (f)-->(g) RETURN g.name AS x ORDER BY x } AS l",
            false
        ),
        strings(&[
            "n='josh' l=[]",
            "n='marko' l=['lop', 'ripple']",
            "n='peter' l=[]",
            "n='vadas' l=[]"
        ])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person {name: 'marko'}) RETURN COLLECT { MATCH (p)-->(f) CALL (f) { \
             MATCH (f)-->(g) RETURN g } RETURN g.name ORDER BY g.name } AS l",
            false
        ),
        ["l=['lop', 'ripple']"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person {name: 'marko'}) RETURN COLLECT { MATCH (p)-->(f) RETURN COUNT { \
             (f)-->() } AS k ORDER BY k } AS l",
            false
        ),
        ["l=[i:0, i:0, i:2]"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, COLLECT { MATCH (p)-->(f) RETURN count(f) } AS l",
            false
        ),
        strings(&[
            "n='josh' l=[i:2]",
            "n='marko' l=[i:3]",
            "n='peter' l=[i:1]",
            "n='vadas' l=[i:0]"
        ])
    );
    // Uncorrelated bodies, a query starting with one.
    assert_eq!(
        table(
            &graph,
            "RETURN COUNT { MATCH (a)-->(b) } AS c, COLLECT { MATCH (s:software) RETURN s.name \
             ORDER BY s.name } AS l",
            false
        ),
        ["c=i:6 l=['lop', 'ripple']"]
    );
    // In ORDER BY, an aggregate's argument, a FOR list, a LET.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n ORDER BY size(COLLECT { MATCH (p)--(f) RETURN \
             f }) DESC, n",
            true
        ),
        ["n='josh'", "n='marko'", "n='peter'", "n='vadas'"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN sum(size(COLLECT { MATCH (p)-->(f) RETURN f })) AS t",
            false
        ),
        ["t=i:6"]
    );
    assert_eq!(
        multiset(
            &graph,
            "MATCH (p:person {name: 'josh'}) FOR x IN COLLECT { MATCH (p)-->(f) RETURN f.name } \
             RETURN x",
            "x"
        ),
        texts(&["lop", "ripple"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) LET l = COLLECT { MATCH (p)-[:created]->(s) RETURN s.name } \
             FILTER size(l) > 1 RETURN p.name AS n, l",
            false
        ),
        ["n='josh' l=['ripple', 'lop']"]
    );
}

/// `COLLECT { }` needs exactly one returned column (42002, named in the error).
#[test]
fn collect_needs_one_column() {
    let graph = modern();
    let err = error(
        &graph,
        "MATCH (p:person) RETURN COLLECT { MATCH (p)-->(f) RETURN f.name, f.age } AS l",
    );
    assert!(err.contains("42002"), "{err}");
    assert!(err.contains("exactly one column"), "{err}");
}

/// Bulk: merged incoming rows give the same `COUNT` / `COLLECT` / whole-query `EXISTS` results
/// as unmerged ones (each row runs its body on one unit of itself).
#[test]
fn whole_query_bodies_keep_bulk_semantics() {
    let graph = modern();
    for query in [
        "MATCH ()<-[:knows]-()-[:created]->(s) RETURN s.name AS n, COUNT { MATCH (s)<-[:created]-(c) \
         RETURN c } AS x",
        "MATCH ()<-[:knows]-()-[:created]->(s) RETURN s.name AS n, COLLECT { MATCH (s)<-[:created]-(c) \
         RETURN c.name ORDER BY c.name } AS x",
        "MATCH ()<-[:knows]-()-[:created]->(s) RETURN s.name AS n, COLLECT { MATCH (s)<-[:created]-(c) \
         RETURN count(c) AS k } AS x",
        "MATCH ()<-[:knows]-()-[:created]->(s) WHERE EXISTS { MATCH (s)<-[:created]-(c) WITH c \
         WHERE c.age > 33 RETURN c } RETURN s.name AS n, 1 AS x",
        "MATCH (a)-->(b) RETURN b.name AS n, COUNT { MATCH (b)<--(c) RETURN c UNION ALL \
         MATCH (b)-->(c) RETURN c } AS x",
    ] {
        let run = |merge: bool| {
            let result = graph
                .read()
                .traversal()
                .with("bulk.merge", merge)
                .gql(query)
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {e}"));
            let mut out: Vec<String> = result.iter().map(|row| format!("{row:?}")).collect();
            out.sort();
            out
        };
        let merged = run(true);
        assert!(!merged.is_empty(), "{query}");
        assert_eq!(merged, run(false), "{query}");
    }
}

/// The plan: a whole-query body is lowered from the host's site like a `CALL` body (no unit
/// row, its `RETURN` a `gql:project` in the child); the top-level `[NOT] EXISTS` conjunct
/// keeps the fast path (`filter()` / `not()`); `COLLECT` closes its child with `gql:value` of
/// the returned column, `COUNT` with `count()`.
#[test]
fn whole_query_bodies_are_children_of_their_host() {
    let graph = modern();
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WITH s WHERE s.lang = \
         'java' RETURN s } RETURN p.name AS n",
    );
    assert_eq!(
        steps(&text),
        [
            r#"v(labels: ["person"])"#,
            r#"filter(__.out("created").has("lang", P.eq("java")).gql:project("s"))"#,
            r#"\> out("created")"#,
            r#"\> has("lang", P.eq("java"))"#,
            r#"\> gql:project("s")"#,
            r#"gql:project("n")"#,
        ],
        "{text}"
    );
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE NOT EXISTS { MATCH (p)-->(f) RETURN f LIMIT 1 } RETURN p.name AS n",
    );
    assert!(
        steps(&text).iter().any(|s| s.starts_with("not(__.out()")),
        "{text}"
    );
    let text = profile(
        &graph,
        "MATCH (p:person) RETURN COLLECT { MATCH (p)-[:created]->(s) RETURN s.name ORDER BY \
         s.name } AS l",
    );
    assert_eq!(
        steps(&text),
        [
            r#"v(labels: ["person"])"#,
            r#"gql:project("l")"#,
            r#"\> out("created")"#,
            r#"\> gql:project("s.name")"#,
            r#"\> gql:order("s.name")"#,
            r#"\> gql:value("s.name")"#,
        ],
        "{text}"
    );
    let text = profile(
        &graph,
        "MATCH (p:person) RETURN COUNT { MATCH (p)-[:created]->(s) RETURN DISTINCT s.lang } AS c",
    );
    let all = steps(&text);
    assert_eq!(
        all.last().map(String::as_str),
        Some(r#"\> count()"#),
        "{text}"
    );
    assert!(all.contains(&r#"\> gql:distinct()"#.to_string()), "{text}");
}

/// The list a `COLLECT { }` builds is checked against `traversal.max_materialized_bytes`
/// before it is built, like a pattern comprehension's (an error, never a cut list, D14).
#[test]
fn collected_lists_obey_the_limits() {
    let graph = modern();
    let query = "MATCH (p:person) RETURN COLLECT { MATCH (p)-[]-{1,3}(f) RETURN f.name } AS l";
    let err = graph
        .read()
        .traversal()
        .with("traversal.max_materialized_bytes", 300)
        .gql(query)
        .to_list()
        .expect_err(query);
    let cause = err.root_cause().to_string();
    assert!(
        cause.contains("traversal.max_materialized_bytes"),
        "{cause}"
    );
    assert!(
        graph
            .read()
            .traversal()
            .with("traversal.max_materialized_bytes", 100_000)
            .gql(query)
            .to_list()
            .is_ok()
    );
}
