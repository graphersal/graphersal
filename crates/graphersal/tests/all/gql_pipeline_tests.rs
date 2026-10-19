//! GQL stage 1b pipeline (feature `gql`): `LET`, `FILTER`, a projecting `WITH` (substitutions
//! and conjuncts, no steps of their own), a materializing `WITH` (`DISTINCT`, `ORDER BY`,
//! `OFFSET`, `LIMIT`, its `WHERE`, continuing a pattern from its columns) and `OPTIONAL MATCH`
//! (`optional()`, a leading one from one unit row). Differential tests pair each query with a
//! hand-written Gremlin twin of the same NULL semantics and compare the rows as multisets, or in
//! order when the query sorts (design D18); the rest checks the lowered plan in `.profile()`.
#![cfg(feature = "gql")]

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

/// The column `key` of every row, as text (`null` for NULL), in row order.
fn texts(rows: &[ElementProperty], key: &str) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let value = match row {
                ElementProperty::Map(map) => map.get_str(key).cloned(),
                ElementProperty::Object(map) => map.get(key).cloned(),
                other => panic!("not a row map: {other:?}"),
            };
            match value.unwrap_or_else(|| panic!("no column {key} in {row:?}")) {
                ElementProperty::Null => "null".to_string(),
                ElementProperty::String(s) => s.to_string(),
                ElementProperty::Number(n) => match n.as_i64() {
                    Some(i) => i.to_string(),
                    None => n.as_f64().unwrap_or_default().to_string(),
                },
                other => format!("{other:?}"),
            }
        })
        .collect()
}

// ----- substitutions plan like stage 1a -----

#[test]
fn let_filter_and_a_projecting_with_lower_like_the_stage_1a_query() {
    let graph = TraversalGraph::tinkerpop_modern();
    let pairs = [
        (
            "MATCH (p:person) LET a = p.age FILTER a > 30 RETURN p.name AS n",
            "MATCH (p:person) WHERE p.age > 30 RETURN p.name AS n",
        ),
        (
            "MATCH (p:person) FILTER WHERE p.name = 'marko' MATCH (p)-[:knows]->(f) RETURN f.name AS n",
            "MATCH (p:person {name: 'marko'})-[:knows]->(f) RETURN f.name AS n",
        ),
        (
            "MATCH (p:person) WITH p AS q WHERE q.age >= 32 RETURN q.name AS n",
            "MATCH (p:person) WHERE p.age >= 32 RETURN p.name AS n",
        ),
        (
            "MATCH (p:person)-[:created]->(s) WITH * RETURN s.name AS n",
            "MATCH (p:person)-[:created]->(s) RETURN s.name AS n",
        ),
    ];
    for (query, twin) in pairs {
        assert!(steps(&profile(&graph, twin)).len() > 2, "{twin}");
        assert_eq!(
            steps(&profile(&graph, query)),
            steps(&profile(&graph, twin)),
            "\n{query}\n{twin}"
        );
        let mut left = texts(&rows(&graph, query), "n");
        let mut right = texts(&rows(&graph, twin), "n");
        left.sort();
        right.sort();
        assert_eq!(left, right, "{query}");
    }
}

#[test]
fn distinct_with_and_return_use_gql_equality() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .add_v(vec!["person"])
        .property("name", "floatage")
        .property("age", ElementProperty::from(32.0))
        .iterate()
        .unwrap();
    // 32 and 32.0 are one value (decision C3): josh's row arrives first and is kept.
    let query = "MATCH (p:person) WHERE p.age >= 32 WITH DISTINCT p.age AS a RETURN a ORDER BY a";
    assert_eq!(texts(&rows(&graph, query), "a"), ["32", "35"]);
    let text = profile(&graph, query);
    assert!(text.contains("gql:distinct()"), "{text}");
    assert!(!text.contains("dedup()"), "{text}");
}

// ----- the plans of the new forms -----

#[test]
fn a_materializing_with_labels_its_row_only_when_read_after_leaving_it() {
    let graph = TraversalGraph::tinkerpop_modern();
    // Read on its row only: no row label.
    let text = profile(
        &graph,
        "MATCH (p:person) WITH p.name AS n ORDER BY n LIMIT 3 WHERE n <> 'josh' RETURN n",
    );
    assert!(!text.contains("as(\"#w"), "{text}");
    assert_eq!(
        steps(&text),
        [
            "v(labels: [\"person\"])",
            "gql:project(\"n\")",
            "gql:order(\"n\")",
            "limit(3)",
            "gql:filter(\"n <> 'josh'\")",
            "gql:project(\"n\")",
        ]
    );
    // A later MATCH continues from a column: gql:bind reads the row it stands on, the RETURN
    // reads the column `a` after the pattern left the row: the row label.
    let text = profile(
        &graph,
        "MATCH (p:person) WITH p, p.age AS a ORDER BY a LIMIT 2 MATCH (p)-[:knows]->(f) RETURN a, f.name AS f",
    );
    let plan = steps(&text);
    assert!(plan.iter().any(|s| s.starts_with("as(\"#w1\")")), "{text}");
    assert!(
        plan.iter().any(|s| s.starts_with("gql:bind(\"p\")")),
        "{text}"
    );
    assert!(!text.contains("select("), "{text}");
    // Only the column read after leaving needs the label; no pattern binding is labelled.
    assert!(!text.contains("as(\"p\")"), "{text}");
    assert!(!text.contains("as(\"f\")"), "{text}");
    assert!(!text.contains("full"), "{text}");
}

#[test]
fn an_optional_match_is_an_optional_child_labelled_only_where_read() {
    let graph = TraversalGraph::tinkerpop_modern();
    // `s` is read after the optional part, `p` too; nothing else is labelled.
    let text = profile(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS n, s.name AS s",
    );
    assert!(
        text.contains("optional(__.out(\"created\").as(\"s\"))"),
        "{text}"
    );
    assert!(text.contains("as(\"p\")"), "{text}");
    // Unread: no label inside the part.
    let text = profile(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS n",
    );
    assert!(text.contains("optional(__.out(\"created\"))"), "{text}");
    assert!(!text.contains("as(\"s\")"), "{text}");
    // A leading OPTIONAL MATCH starts from one unit row.
    let text = profile(&graph, "OPTIONAL MATCH (x:software) RETURN x.name AS n");
    let plan = steps(&text);
    assert!(
        plan.first().is_some_and(|s| s.starts_with("inject(0)")),
        "{text}"
    );
    assert!(
        plan.get(1)
            .is_some_and(|s| s.starts_with("optional(__.v(labels: [\"software\"]).as(\"x\"))")),
        "{text}"
    );
    // Away from the current element: the part starts with select().
    let text = profile(
        &graph,
        "MATCH (p:person)-[:knows]->(f) OPTIONAL MATCH (p)-[:created]->(s) RETURN f.name AS f, s.name AS s",
    );
    assert!(text.contains("optional(__.select(\"p\")"), "{text}");
}

// ----- results -----

#[test]
fn optional_match_rows_and_nulls() {
    let graph = TraversalGraph::tinkerpop_modern();
    let result = rows(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS p, s.name AS s ORDER BY p, s",
    );
    let pairs: Vec<(String, String)> = texts(&result, "p")
        .into_iter()
        .zip(texts(&result, "s"))
        .collect();
    let expected = [
        ("josh", "lop"),
        ("josh", "ripple"),
        ("marko", "lop"),
        ("peter", "lop"),
        ("vadas", "null"),
    ];
    assert_eq!(
        pairs,
        expected
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect::<Vec<_>>()
    );
    // A leading OPTIONAL MATCH that matches nothing gives one row of NULLs.
    let result = rows(&graph, "OPTIONAL MATCH (x:nothing) RETURN x, x.name AS n");
    assert_eq!(texts(&result, "x"), ["null"]);
    assert_eq!(texts(&result, "n"), ["null"]);
    // ... and one row per match otherwise.
    let result = rows(
        &graph,
        "OPTIONAL MATCH (x:software) RETURN x.name AS n ORDER BY n",
    );
    assert_eq!(texts(&result, "n"), ["lop", "ripple"]);
    // Its WHERE (over outer and inner variables) belongs to the optional part.
    let result = rows(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WHERE s.lang = 'java' AND p.age > 30 \
         RETURN p.name AS p, s.name AS s ORDER BY p, s",
    );
    assert_eq!(
        texts(&result, "s"),
        ["lop", "ripple", "null", "lop", "null"]
    );
    // FILTER after it sees the NULLs.
    let result = rows(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NULL RETURN p.name AS n",
    );
    assert_eq!(texts(&result, "n"), ["vadas"]);
    // A later MATCH from an optional variable drops the rows where it is NULL.
    let result = rows(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) MATCH (s)<-[:created]-(q) \
         RETURN p.name AS p, q.name AS q",
    );
    assert_eq!(result.len(), 10);
    assert!(!texts(&result, "p").contains(&"vadas".to_string()));
    // Two optional parts.
    let result = rows(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:knows]->(f) OPTIONAL MATCH (p)-[:created]->(s) \
         RETURN p.name AS p, f.name AS f, s.name AS s ORDER BY p, f, s",
    );
    assert_eq!(
        texts(&result, "f"),
        ["null", "null", "josh", "vadas", "null", "null"]
    );
    assert_eq!(
        texts(&result, "s"),
        ["lop", "ripple", "lop", "lop", "lop", "null"]
    );
}

#[test]
fn a_materializing_with_orders_limits_filters_and_continues() {
    let graph = TraversalGraph::tinkerpop_modern();
    // ORDER BY ... SKIP ... LIMIT, then its WHERE on the rows left (after LIMIT).
    let result = rows(
        &graph,
        "MATCH (p:person) WITH p.name AS n, p.age AS a ORDER BY a SKIP 1 LIMIT 2 WHERE a > 28 \
         RETURN n ORDER BY n",
    );
    assert_eq!(texts(&result, "n"), ["josh", "marko"]);
    let result = rows(
        &graph,
        "MATCH (p:person) WITH p.name AS n, p.age AS a ORDER BY a LIMIT 2 WHERE a > 28 RETURN n",
    );
    assert_eq!(texts(&result, "n"), ["marko"]);
    // Continue the pattern from a column; read another column after leaving the row.
    let result = rows(
        &graph,
        "MATCH (p:person) WITH p, p.age AS a ORDER BY a LIMIT 2 MATCH (p)-[:knows]->(f) \
         RETURN a, f.name AS f ORDER BY f",
    );
    assert_eq!(texts(&result, "a"), ["29", "29"]);
    assert_eq!(texts(&result, "f"), ["josh", "vadas"]);
    // A column holding NULL (an OPTIONAL MATCH that did not match) continues nowhere.
    let result = rows(
        &graph,
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WITH p, s ORDER BY p.name LIMIT 10 \
         MATCH (s)<-[:created]-(q) RETURN p.name AS p, q.name AS q",
    );
    assert_eq!(result.len(), 10);
    // An OPTIONAL MATCH from a column of the row.
    let result = rows(
        &graph,
        "MATCH (p:person) WITH p ORDER BY p.name LIMIT 2 OPTIONAL MATCH (p)-[:created]->(s) \
         RETURN p.name AS p, s.name AS s ORDER BY p, s",
    );
    assert_eq!(texts(&result, "p"), ["josh", "josh", "marko"]);
    assert_eq!(texts(&result, "s"), ["lop", "ripple", "lop"]);
    // WITH after WITH: the second one reads the first one's row.
    let result = rows(
        &graph,
        "MATCH (p:person) WITH p.name AS n ORDER BY n LIMIT 3 WITH n ORDER BY n DESC LIMIT 1 RETURN n",
    );
    assert_eq!(texts(&result, "n"), ["peter"]);
    // A name dropped by WITH and bound again is a new variable.
    let result = rows(
        &graph,
        "MATCH (p:person {name: 'marko'})-[:knows]->(x) WITH p ORDER BY p.name LIMIT 1 \
         MATCH (p)-[:created]->(x) RETURN x.name AS x",
    );
    assert_eq!(texts(&result, "x"), ["lop"]);
}

#[test]
fn joins_and_trail_against_with_columns() {
    let graph = TraversalGraph::tinkerpop_modern();
    // A join: both ends of the new edge are columns of the row.
    let result = rows(
        &graph,
        "MATCH (a:person), (b:person) WITH a, b ORDER BY a.name, b.name LIMIT 100 \
         MATCH (a)-[:knows]->(b) RETURN a.name AS a, b.name AS b ORDER BY b",
    );
    assert_eq!(texts(&result, "a"), ["marko", "marko"]);
    assert_eq!(texts(&result, "b"), ["josh", "vadas"]);
    // An edge join with an edge column.
    let result = rows(
        &graph,
        "MATCH ()-[e:knows]->() WITH e ORDER BY e.weight LIMIT 1 MATCH (a)-[e]->(b) \
         RETURN a.name AS a, b.name AS b",
    );
    assert_eq!(texts(&result, "b"), ["vadas"]);
    // Trail: another edge of the clause differs from the edge column.
    let result = rows(
        &graph,
        "MATCH ()-[e:knows]->() WITH e LIMIT 5 MATCH (a)-[e]->(b), (a)-[f:knows]->(c) \
         RETURN b.name AS b, c.name AS c ORDER BY b",
    );
    assert_eq!(texts(&result, "b"), ["josh", "vadas"]);
    assert_eq!(texts(&result, "c"), ["vadas", "josh"]);
}

#[test]
fn order_by_reads_an_alias_only_where_the_key_names_it() {
    let graph = TraversalGraph::tinkerpop_modern();
    // `x` stands for `a.age`; the alias `a` must not capture the variable `a` inside it.
    for query in [
        "MATCH (a:person) LET x = a.age RETURN a.name AS a ORDER BY x",
        "MATCH (a:person) WITH a AS b RETURN b.name AS a ORDER BY b.age",
    ] {
        assert_eq!(
            texts(&rows(&graph, query), "a"),
            ["vadas", "marko", "josh", "peter"],
            "{query}"
        );
    }
    // A key naming the alias still reads the column.
    let result = rows(
        &graph,
        "MATCH (a:person) RETURN a.age AS a, a.name AS n ORDER BY a * -1",
    );
    assert_eq!(texts(&result, "n"), ["peter", "josh", "marko", "vadas"]);
}

// ----- differential tests against Gremlin twins (Rhai) -----

#[cfg(feature = "script")]
mod differential {
    use std::sync::Arc;

    use graphersal::prelude::*;

    /// Runs the GQL query and its Gremlin twin on `graph`; compares the rows as multisets,
    /// or in order when `ordered`.
    fn same(graph: &Arc<Graph>, query: &str, twin: &str, ordered: bool) {
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
            if !ordered {
                out.sort();
            }
            out
        };
        assert_eq!(text(&left), text(&right), "\nGQL:  {query}\ntwin: {twin}");
        assert!(!left.is_empty(), "{query}: no rows to compare");
    }

    /// The modern graph plus persons with a stored `null` age, a float age, a string age, no
    /// age, and a second label (the NULL fixture of `gql_tests`).
    fn fixture() -> Arc<Graph> {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let _ = crate::script_host::eval_value(
            graph.clone(),
            r#"g.addV("person").property("name", "nullage").property("age", ()).iterate();
               g.addV("person").property("name", "floatage").property("age", 32.0).iterate();
               g.addV("person").property("name", "strage").property("age", "x").iterate();
               g.addV("person").property("name", "noage").iterate();
               g.addV(["person", "employee"]).property("name", "emma").property("age", 41).iterate();"#,
        )
        .unwrap();
        graph
    }

    /// `coalesce(<t>, constant(()))`: a value or NULL.
    fn or_null(t: &str) -> String {
        format!("__.coalesce({t}, __.constant(()))")
    }

    #[test]
    fn let_filter_and_projecting_with_match_their_twins() {
        let graph = fixture();
        let name = or_null(r#"__.values("name")"#);
        let cases: Vec<(String, String)> = vec![
            (
                "MATCH (p:person) FILTER p.age >= 32 RETURN p.name AS n".into(),
                format!(r#"g.V().hasLabel("person").has("age", P.gte(32)).project("n").by({name})"#),
            ),
            (
                "MATCH (p:person) LET a = p.age FILTER a IS NULL RETURN p.name AS n".into(),
                format!(
                    r#"g.V().hasLabel("person").or(__.hasNot("age"), __.has("age", P.eq(()))).project("n").by({name})"#
                ),
            ),
            (
                "MATCH (p:person) LET a = p.age, n = p.name FILTER a < 30 RETURN n, a".into(),
                r#"g.V().hasLabel("person").has("age", P.lt(30)).project("n", "a").by("name").by("age")"#.into(),
            ),
            (
                "MATCH (p:person)-[:knows]->(f) WITH p.name AS a, f WHERE f.age > 30 RETURN a, f.name AS b"
                    .into(),
                r#"g.V().hasLabel("person").as("a").out("knows").has("age", P.gt(30)).as("b").select("a", "b").by("name")"#.into(),
            ),
            (
                "MATCH (p:person) WITH * WHERE p.age < 30 RETURN p.name AS n".into(),
                format!(r#"g.V().hasLabel("person").has("age", P.lt(30)).project("n").by({name})"#),
            ),
            (
                "MATCH (p:person) FILTER p.name STARTS WITH 'n' LET q = p MATCH (q)-->(x) RETURN x".into(),
                "g.V().hasLabel(\"person\").has(\"name\", TextP.starting_with(\"n\")).out().project(\"x\").by(__.identity())".into(),
            ),
        ];
        // The last twin is empty: no 'n' person has an edge.
        for (query, twin) in &cases[..cases.len() - 1] {
            same(&graph, query, twin, false);
        }
        let lock = graph.read();
        let (query, _) = &cases[cases.len() - 1];
        assert!(lock.traversal().gql(query).to_list().unwrap().is_empty());
    }

    #[test]
    fn materializing_with_matches_its_twins() {
        let graph = fixture();
        let modern = Arc::new(GraphSource::tinkerpop_modern());
        // DISTINCT under GQL equality: floatage's 32.0 is josh's 32 (first arrival kept).
        same(
            &graph,
            "MATCH (p:person) WITH DISTINCT p.age AS a RETURN a",
            &format!(
                r#"g.V().hasLabel("person").not(__.has("name", "floatage")).project("a").by({}).dedup()"#,
                or_null(r#"__.values("age")"#)
            ),
            false,
        );
        same(
            &modern,
            "MATCH (p:person) WITH p ORDER BY p.age DESC LIMIT 2 MATCH (p)-[:created]->(s) \
             RETURN p.name AS p, s.name AS s",
            r#"g.V().hasLabel("person").order().by("age", Order.desc).limit(2).as("p").out("created").as("s").select("p", "s").by("name")"#,
            false,
        );
        same(
            &modern,
            "MATCH (p:person) WITH p.name AS n, p.age AS a ORDER BY a SKIP 1 LIMIT 2 RETURN n, a",
            r#"g.V().hasLabel("person").order().by("age").range(1, 3).project("n", "a").by("name").by("age")"#,
            true,
        );
        same(
            &modern,
            "MATCH (p:person) WITH p, p.age AS a ORDER BY a LIMIT 2 MATCH (p)-[:knows]->(f) \
             RETURN a, f.name AS f",
            r#"g.V().hasLabel("person").order().by("age").limit(2).as("p").out("knows").as("f").project("a", "f").by(__.select("p").values("age")).by(__.select("f").values("name"))"#,
            false,
        );
        same(
            &modern,
            "MATCH (a:person), (b:person) WITH a, b ORDER BY a.name, b.name LIMIT 100 \
             MATCH (a)-[:knows]->(b) RETURN a.name AS a, b.name AS b",
            r#"g.V().hasLabel("person").as("a").out("knows").hasLabel("person").as("b").select("a", "b").by("name")"#,
            false,
        );
    }

    #[test]
    fn optional_match_matches_its_twins() {
        let graph = fixture();
        let p = r#"__.select("p").values("name")"#;
        let s = or_null(r#"__.select("s").values("name")"#);
        let cases: Vec<(String, String)> = vec![
            (
                "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS p, s.name AS s"
                    .into(),
                format!(
                    r#"g.V().hasLabel("person").as("p").optional(__.out("created").as("s")).project("p", "s").by({p}).by({s})"#
                ),
            ),
            (
                "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WHERE s.lang = 'java' AND p.age > 30 \
                 RETURN p.name AS p, s.name AS s"
                    .into(),
                format!(
                    r#"g.V().hasLabel("person").as("p").optional(__.has("age", P.gt(30)).out("created").has("lang", "java").as("s")).project("p", "s").by({p}).by({s})"#
                ),
            ),
            (
                "MATCH (p:person)-[:knows]->(f) OPTIONAL MATCH (p)-[:created]->(s) \
                 RETURN f.name AS p, s.name AS s"
                    .into(),
                format!(
                    r#"g.V().hasLabel("person").as("x").out("knows").as("p").optional(__.select("x").out("created").as("s")).project("p", "s").by({p}).by({s})"#
                ),
            ),
            (
                "OPTIONAL MATCH (p:software) RETURN p.name AS p".into(),
                r#"g.V().hasLabel("software").project("p").by("name")"#.into(),
            ),
            (
                "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NULL RETURN p.name AS p"
                    .into(),
                r#"g.V().hasLabel("person").not(__.out("created")).project("p").by("name")"#.into(),
            ),
            (
                "MATCH (p:person) OPTIONAL MATCH (p)<-[:knows]-(k) RETURN p.name AS p, k.name AS s".into(),
                format!(
                    r#"g.V().hasLabel("person").as("p").optional(__.in("knows").as("s")).project("p", "s").by({p}).by({s})"#
                ),
            ),
        ];
        for (query, twin) in &cases {
            same(&graph, query, twin, false);
        }
    }
}
