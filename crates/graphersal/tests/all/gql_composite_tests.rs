//! GQL stage 2d composite queries (feature `gql`): `UNION` / `UNION DISTINCT` / `UNION ALL` and
//! `NEXT` (design item 8, E15, E19). A `UNION` block lowers to `union(__.<operand>, ..)` (the
//! start step `g.union(..)` at the start of the query, a mid-traversal `union()` after `NEXT`),
//! each operand lowered like a query of its own (own pushdown and index), then `gql:distinct()`
//! for `UNION` / `UNION DISTINCT`. Differential twins against Gremlin `g.union(..)` (Rhai) with
//! `dedup()` where GQL equality agrees with it compare the rows as multisets (design D18);
//! GQL equality (`1 = 1.0`, `NULL`s equal), column reordering, per-operand `ORDER BY` / `LIMIT`
//! and `NEXT` chains are hand-computed rows.
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

// ----- still refused -----

#[test]
fn call_inside_a_union_is_still_refused() {
    let graph = modern();
    let text = error(
        &graph,
        "RETURN 1 AS x UNION ALL CALL { RETURN 2 AS y } RETURN y AS x",
    );
    assert!(
        text.contains("CALL { } (a subquery) is not yet supported"),
        "{text}"
    );
    let text = error(&graph, "RETURN 1 AS x UNION ALL RETURN 2 AS y");
    assert!(text.contains("42002"), "{text}");
}
