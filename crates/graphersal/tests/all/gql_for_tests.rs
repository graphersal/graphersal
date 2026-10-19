//! GQL stage 2a `FOR` / `UNWIND` and queries that start without `MATCH` (feature `gql`):
//! `gql:for` per list item (design mechanism 2), the unit row `inject(0)` (design M1, E4),
//! `NULL` / non-list lists (E1), duplicates and `NULL` items, `WITH ORDINALITY` / `WITH OFFSET`,
//! a `FOR` item continued as a node or edge of a later `MATCH` through `gql:bind` (E3), `FOR`
//! around `OPTIONAL MATCH`, bulk, and the plans (a row label only when the item is read after
//! the row was left). Differential twins against Gremlin (Rhai) compare the rows as multisets
//! (design D18); the `NULL` cases are hand-computed rows.
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

fn rows_with(
    graph: &Arc<Graph>,
    query: &str,
    params: Vec<(&str, ElementProperty)>,
) -> Vec<ElementProperty> {
    graph
        .read()
        .traversal()
        .gql_with(query, params)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
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

fn column<'r>(row: &'r ElementProperty, key: &str) -> &'r ElementProperty {
    match row {
        ElementProperty::Object(map) => map
            .get(key)
            .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
        other => panic!("not a row map: {other:?}"),
    }
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

/// Every row as `a|b|..` over `keys`, in result order.
fn line(rows: &[ElementProperty], keys: &[&str]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            keys.iter()
                .map(|k| norm(column(row, k)))
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

/// Every row of `query` as `a|b|..` over `keys`, sorted (a multiset).
fn table(graph: &Arc<Graph>, query: &str, keys: &[&str]) -> Vec<String> {
    let mut out = line(&rows(graph, query), keys);
    out.sort();
    out
}

/// Every row of `query` as `a|b|..` over `keys`, in result order.
fn ordered(graph: &Arc<Graph>, query: &str, keys: &[&str]) -> Vec<String> {
    line(&rows(graph, query), keys)
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

fn ids(items: &[&str]) -> ElementProperty {
    ElementProperty::Array(items.iter().map(|s| ElementProperty::from(*s)).collect())
}

// ----- differential twins against Gremlin (Rhai) -----

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

    /// The `x` column of the GQL script `gql` (`g.gql(..)`, selected) against the values of the
    /// Gremlin twin, as multisets.
    fn twin(graph: &Arc<Graph>, gql: &str, gremlin: &str) {
        let left = values(graph, &format!("{gql}.select(\"x\")"));
        let right = values(graph, gremlin);
        assert!(!left.is_empty(), "{gql}: no rows to compare");
        assert_eq!(left, right, "\nGQL:  {gql}\ntwin: {gremlin}");
    }

    #[test]
    fn for_and_unwind_match_flat_map_constant_unfold() {
        let graph = modern();
        for (gql, gremlin) in [
            (
                r#"g.gql("FOR x IN [1, 2, 2, 3] RETURN x")"#,
                "g.inject(0).flat_map(__.constant([1, 2, 2, 3]).unfold())",
            ),
            (
                r#"g.gql("UNWIND ['a', 'b', 'a'] AS x RETURN x")"#,
                r#"g.inject(0).flat_map(__.constant(["a", "b", "a"]).unfold())"#,
            ),
            (
                r#"g.gql("MATCH (p:person) FOR y IN [1, 2, 3] RETURN p.name AS x")"#,
                r#"g.V().hasLabel("person").as("p").flat_map(__.constant([1, 2, 3]).unfold()).select("p").values("name")"#,
            ),
            (
                r#"g.gql("MATCH (p:person) FOR x IN [p.name, p.age] RETURN x")"#,
                r#"g.V().hasLabel("person").flat_map(__.values("name", "age"))"#,
            ),
            (
                r#"g.gql("MATCH (p:person)-[:created]->(s) WITH p, collect(s) AS ss FOR t IN ss RETURN t.name AS x")"#,
                r#"g.V().hasLabel("person").out("created").values("name")"#,
            ),
            (
                r#"g.gql("FOR n IN $ids MATCH (p:person {name: n}) RETURN p.age AS x", #{ids: ["marko", "josh", "nobody"]})"#,
                r#"g.V().has("person", "name", P.within(["marko", "josh", "nobody"])).values("age")"#,
            ),
            (
                r#"g.gql("MATCH (a:person {name: 'marko'}) FOR i IN [1, 2, 3] MATCH (a)-[:knows]->(b) RETURN b.name AS x")"#,
                r#"g.V().has("person", "name", "marko").out("knows").values("name").as("n").flat_map(__.constant([1, 2, 3]).unfold()).select("n")"#,
            ),
            (
                r#"g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b) FOR n IN nodes(p) MATCH (n)-[:created]->(s) RETURN s.name AS x")"#,
                r#"g.V().has("name", "marko").out("knows").path().unfold().out("created").values("name")"#,
            ),
            (
                r#"g.gql("FOR x IN [1, 2] WITH ORDINALITY i RETURN x * 10 + i AS x")"#,
                "g.inject(0).flat_map(__.constant([11, 22]).unfold())",
            ),
        ] {
            twin(&graph, gql, gremlin);
        }
    }
}

// ----- semantics: E1, duplicates, NULL items, ordinals -----

#[test]
fn for_over_null_gives_no_row_and_over_a_non_list_one_row() {
    let graph = modern();
    assert!(rows(&graph, "FOR x IN null RETURN x").is_empty());
    assert!(rows(&graph, "UNWIND null AS x RETURN x").is_empty());
    assert!(rows(&graph, "FOR x IN [] RETURN x").is_empty());
    assert_eq!(table(&graph, "FOR x IN 5 RETURN x", &["x"]), ["i:5"]);
    assert_eq!(table(&graph, "UNWIND 'a' AS x RETURN x", &["x"]), ["'a'"]);
    assert_eq!(
        table(&graph, "FOR x IN {a: 1} RETURN x.a AS a", &["a"]),
        ["i:1"]
    );
    // A missing property is NULL: no row for that person.
    assert_eq!(
        table(
            &graph,
            "MATCH (n) FOR x IN n.age RETURN n.name AS n, x",
            &["n", "x"]
        ),
        sorted(&[
            "'josh'|i:32",
            "'marko'|i:29",
            "'peter'|i:35",
            "'vadas'|i:27"
        ])
    );
    // A parameter: NULL and a non-list too.
    assert!(
        rows_with(
            &graph,
            "FOR x IN $l RETURN x",
            vec![("l", ElementProperty::Null)]
        )
        .is_empty()
    );
    assert_eq!(
        line(
            &rows_with(
                &graph,
                "FOR x IN $l RETURN x",
                vec![("l", ElementProperty::from(7))]
            ),
            &["x"]
        ),
        ["i:7"]
    );
}

#[test]
fn duplicates_and_null_items_are_rows_of_their_own_in_list_order() {
    let graph = modern();
    assert_eq!(
        ordered(&graph, "FOR x IN [3, null, 1, 3, null] RETURN x", &["x"]),
        ["i:3", "null", "i:1", "i:3", "null"]
    );
    assert_eq!(
        ordered(
            &graph,
            "FOR m IN [{a: 1}, null, {a: 2}] RETURN m.a AS a, m IS NULL AS z",
            &["a", "z"]
        ),
        ["i:1|false", "null|true", "i:2|false"]
    );
    // Nested lists stay lists; a second FOR flattens them.
    assert_eq!(
        ordered(
            &graph,
            "FOR x IN [[1, 2], [], [3]] FOR y IN x RETURN x, y",
            &["x", "y"]
        ),
        ["[i:1, i:2]|i:1", "[i:1, i:2]|i:2", "[i:3]|i:3"]
    );
    assert_eq!(
        table(&graph, "FOR x IN [1, 1, 2] RETURN DISTINCT x", &["x"]),
        ["i:1", "i:2"]
    );
    assert_eq!(
        table(
            &graph,
            "FOR x IN [1, null, 1] RETURN x, count(*) AS c",
            &["x", "c"]
        ),
        sorted(&["i:1|i:2", "null|i:1"])
    );
}

#[test]
fn ordinality_counts_from_one_and_offset_from_zero() {
    let graph = modern();
    assert_eq!(
        ordered(
            &graph,
            "FOR x IN ['a', 'b', 'a'] WITH ORDINALITY i RETURN x, i",
            &["x", "i"]
        ),
        ["'a'|i:1", "'b'|i:2", "'a'|i:3"]
    );
    assert_eq!(
        ordered(
            &graph,
            "FOR x IN ['a', 'b'] WITH OFFSET i RETURN x, i",
            &["x", "i"]
        ),
        ["'a'|i:0", "'b'|i:1"]
    );
    // Per row: every row counts its own list.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) WHERE p.age > 30 FOR c IN [p.name, p.age] WITH ORDINALITY i \
             RETURN p.name AS n, c, i",
            &["n", "c", "i"]
        ),
        sorted(&[
            "'josh'|'josh'|i:1",
            "'josh'|i:32|i:2",
            "'peter'|'peter'|i:1",
            "'peter'|i:35|i:2",
        ])
    );
    // A non-list is one row at the first position; NULL no row.
    assert_eq!(
        ordered(&graph, "FOR x IN 9 WITH OFFSET i RETURN x, i", &["x", "i"]),
        ["i:9|i:0"]
    );
    assert!(rows(&graph, "FOR x IN null WITH ORDINALITY i RETURN x, i").is_empty());
    // The ordinal read after a later MATCH left the record (its row label).
    assert_eq!(
        table(
            &graph,
            "FOR n IN ['josh', 'marko'] WITH ORDINALITY i MATCH (p:person {name: n}) \
             RETURN p.age AS a, i",
            &["a", "i"]
        ),
        sorted(&["i:32|i:1", "i:29|i:2"])
    );
}

#[test]
fn list_parameters_drive_one_row_per_item() {
    let graph = modern();
    let found = rows_with(
        &graph,
        "FOR n IN $ids MATCH (p:person {name: n}) RETURN n, p.age AS a",
        vec![("ids", ids(&["marko", "nobody", "josh", "marko"]))],
    );
    let mut found = line(&found, &["n", "a"]);
    found.sort();
    assert_eq!(
        found,
        sorted(&["'josh'|i:32", "'marko'|i:29", "'marko'|i:29"])
    );
    // `element_id()` against a list of ids (a per-row scan until `gql:lookup`).
    let found = rows_with(
        &graph,
        "FOR id IN $ids MATCH (n) WHERE element_id(n) = id RETURN n.name AS x",
        vec![("ids", ids(&["1", "4", "99"]))],
    );
    let mut found = line(&found, &["x"]);
    found.sort();
    assert_eq!(found, sorted(&["'marko'", "'josh'"]));
}

// ----- the unit row (M1, E4) -----

#[test]
fn queries_without_match_start_from_one_unit_row() {
    let graph = modern();
    assert_eq!(ordered(&graph, "RETURN 1 AS x", &["x"]), ["i:1"]);
    assert_eq!(
        ordered(
            &graph,
            "RETURN 1 + 2 AS x, 'a' AS y, null AS z",
            &["x", "y", "z"]
        ),
        ["i:3|'a'|null"]
    );
    assert_eq!(ordered(&graph, "RETURN count(*) AS c", &["c"]), ["i:1"]);
    assert_eq!(
        ordered(&graph, "LET x = 2 RETURN x * 2 AS y", &["y"]),
        ["i:4"]
    );
    assert_eq!(
        ordered(&graph, "LET x = 1 FILTER x > 0 RETURN x", &["x"]),
        ["i:1"]
    );
    assert!(rows(&graph, "LET x = 1 FILTER x > 1 RETURN x").is_empty());
    assert_eq!(ordered(&graph, "WITH 1 AS x RETURN x", &["x"]), ["i:1"]);
    assert_eq!(
        ordered(&graph, "WITH 1 AS x LIMIT 1 RETURN x", &["x"]),
        ["i:1"]
    );
    assert!(rows(&graph, "WITH 1 AS x SKIP 1 RETURN x").is_empty());
    assert_eq!(
        line(
            &rows_with(
                &graph,
                "RETURN $a AS a",
                vec![("a", ElementProperty::from("v"))]
            ),
            &["a"]
        ),
        ["'v'"]
    );
    assert_eq!(
        ordered(&graph, "FOR x IN [] RETURN count(*) AS c", &["c"]),
        ["i:0"]
    );
    // A leading LET then MATCH lowers like the MATCH with the constant (no unit row).
    let text = profile(
        &graph,
        "LET k = 'marko' MATCH (p:person {name: k}) RETURN p.age AS a",
    );
    assert_eq!(
        steps(&text),
        steps(&profile(
            &graph,
            "MATCH (p:person {name: 'marko'}) RETURN p.age AS a"
        ))
    );
    assert_eq!(
        steps(&profile(&graph, "RETURN 1 AS x")),
        ["inject(0)", "gql:project(\"x\")"]
    );
    // An empty graph still has the unit row.
    let empty = Arc::new(Graph::new(TraversalGraph::new()));
    assert_eq!(ordered(&empty, "RETURN 1 AS x", &["x"]), ["i:1"]);
    assert_eq!(
        ordered(&empty, "FOR x IN [1, 2] RETURN x", &["x"]),
        ["i:1", "i:2"]
    );
    // `RETURN *` with nothing in scope stays an error.
    assert!(graph.read().traversal().gql("RETURN *").to_list().is_err());
}

// ----- FOR items as nodes and edges (E3) -----

#[test]
fn a_for_item_continues_a_pattern_through_gql_bind() {
    let graph = modern();
    let query = "MATCH (p:person)-[:created]->(s) WITH collect(s) AS ss \
                 FOR t IN ss MATCH (t)<-[:created]-(q) RETURN t.name AS t, q.name AS q";
    assert_eq!(
        table(&graph, query, &["t", "q"]),
        sorted(&[
            // lop is created by marko, josh, peter, and reached three times.
            "'lop'|'josh'",
            "'lop'|'josh'",
            "'lop'|'josh'",
            "'lop'|'marko'",
            "'lop'|'marko'",
            "'lop'|'marko'",
            "'lop'|'peter'",
            "'lop'|'peter'",
            "'lop'|'peter'",
            "'ripple'|'josh'",
        ])
    );
    let text = profile(&graph, query);
    assert!(text.contains("gql:bind(\"t\")"), "{text}");
    // An edge item: the later pattern checks the edge against it.
    assert_eq!(
        table(
            &graph,
            "MATCH ()-[e:created]->() WHERE e.weight > 0.5 WITH collect(e) AS es \
             FOR r IN es MATCH (a)-[r]->(b) RETURN a.name AS a, b.name AS b",
            &["a", "b"]
        ),
        sorted(&["'josh'|'ripple'"])
    );
    // Items that are not vertices give no row; with an ordinal too.
    assert!(rows(&graph, "FOR n IN [1, null, 'x'] MATCH (n)-->(m) RETURN m").is_empty());
    assert_eq!(
        table(
            &graph,
            "MATCH (v:person {name: 'vadas'}) FOR n IN [v, 1, null] WITH ORDINALITY i \
             MATCH (n)<-[:knows]-(m) RETURN m.name AS m, i",
            &["m", "i"]
        ),
        ["'marko'|i:1"]
    );
}

// ----- FOR and OPTIONAL MATCH -----

#[test]
fn for_flows_through_optional_match() {
    let graph = modern();
    // FOR after OPTIONAL MATCH reads the part's NULLs.
    assert_eq!(
        table(
            &graph,
            "MATCH (a:person) OPTIONAL MATCH (a)-[:created]->(s) FOR x IN [1, 2] \
             RETURN a.name AS a, s.name AS s, x",
            &["a", "s", "x"]
        ),
        sorted(&[
            "'josh'|'lop'|i:1",
            "'josh'|'lop'|i:2",
            "'josh'|'ripple'|i:1",
            "'josh'|'ripple'|i:2",
            "'marko'|'lop'|i:1",
            "'marko'|'lop'|i:2",
            "'peter'|'lop'|i:1",
            "'peter'|'lop'|i:2",
            "'vadas'|null|i:1",
            "'vadas'|null|i:2",
        ])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (a:person {name: 'vadas'}) OPTIONAL MATCH (a)-[:created]->(s) \
             FOR x IN [s.name, a.name] RETURN x",
            &["x"]
        ),
        sorted(&["null", "'vadas'"])
    );
    // OPTIONAL MATCH after FOR reads the item inside the part.
    assert_eq!(
        table(
            &graph,
            "FOR n IN ['marko', 'nobody'] OPTIONAL MATCH (p:person {name: n})-[:created]->(s) \
             RETURN n, s.name AS s",
            &["n", "s"]
        ),
        sorted(&["'marko'|'lop'", "'nobody'|null"])
    );
    // A leading OPTIONAL MATCH that does not match, then FOR.
    assert_eq!(
        table(
            &graph,
            "OPTIONAL MATCH (z:nothing) FOR x IN [1, z] RETURN x",
            &["x"]
        ),
        sorted(&["i:1", "null"])
    );
}

// ----- other stages after FOR -----

#[test]
fn for_rows_feed_the_later_stages() {
    let graph = modern();
    assert_eq!(
        ordered(
            &graph,
            "FOR x IN [3, 1, 2] WITH x ORDER BY x DESC LIMIT 2 RETURN x",
            &["x"]
        ),
        ["i:3", "i:2"]
    );
    assert_eq!(
        ordered(
            &graph,
            "FOR x IN [3, 1, 2] FILTER x > 1 RETURN sum(x) AS s",
            &["s"]
        ),
        ["i:5"]
    );
    assert_eq!(
        ordered(
            &graph,
            "FOR x IN [3, 1, 2] RETURN x ORDER BY x SKIP 1",
            &["x"]
        ),
        ["i:2", "i:3"]
    );
    assert_eq!(
        ordered(
            &graph,
            "FOR x IN [1, 2] LET y = x * 10 FOR z IN [y, y + 1] RETURN collect(z) AS l",
            &["l"]
        ),
        ["[i:10, i:11, i:20, i:21]"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) WITH p.name AS n, p.age AS a ORDER BY a LIMIT 2 \
             FOR c IN [n, a] RETURN n, c",
            &["n", "c"]
        ),
        sorted(&[
            "'vadas'|'vadas'",
            "'vadas'|i:27",
            "'marko'|'marko'",
            "'marko'|i:29"
        ])
    );
}

// ----- bulk -----

#[test]
fn merged_rows_unwind_like_unmerged_ones() {
    let graph = modern();
    let graph = graph.read();
    // Three paths reach lop: a merged run (bulk) unwinds like an unmerged one.
    for query in [
        "MATCH (:person)-[:created]->(s) FOR x IN [s.name, 1] RETURN x, count(*) AS c",
        "MATCH (:person)-[:created]->(s) FOR x IN [1, 2] WITH ORDINALITY i RETURN i, count(*) AS c",
    ] {
        let run = |merge: bool| {
            let rows = graph
                .traversal()
                .with("bulk.merge", merge)
                .gql(query)
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {e}"));
            let mut out: Vec<String> = rows.iter().map(norm).collect();
            out.sort();
            out
        };
        assert_eq!(run(true), run(false), "{query}");
    }
    let rows = graph
        .traversal()
        .gql("MATCH (:person)-[:created]->(s) FOR x IN [s.name, 1] RETURN x, count(*) AS c")
        .to_list()
        .unwrap();
    let mut found = line(&rows, &["x", "c"]);
    found.sort();
    assert_eq!(found, sorted(&["'lop'|i:3", "'ripple'|i:1", "i:1|i:4"]));
}

// ----- plans -----

#[test]
fn the_item_is_labelled_only_when_read_after_leaving_it() {
    let graph = modern();
    // Read on its own row: no label at all.
    assert_eq!(
        steps(&profile(&graph, "FOR x IN [1, 2] RETURN x * 2 AS y")),
        [
            "inject(0)",
            "gql:for(\"x IN [1, 2]\")",
            "gql:project(\"y\")"
        ]
    );
    // Not read after a later MATCH: no label.
    let text = profile(
        &graph,
        "FOR x IN [1, 2] MATCH (p:person) RETURN p.name AS n",
    );
    assert!(!text.contains("as(\""), "{text}");
    // Read after a later MATCH: `as("x")` on the item.
    assert_eq!(
        steps(&profile(
            &graph,
            "FOR x IN [1, 2] MATCH (p:person) RETURN p.name AS n, x"
        )),
        [
            "inject(0)",
            "gql:for(\"x IN [1, 2]\")",
            "as(\"x\") [path: labels(x)]",
            "v() [path: labels(x)]",
            "has_label(\"person\") [path: labels(x)]",
            "gql:project(\"n\", \"x\")"
        ]
    );
    // With an ordinal the record is labelled `#f<k>`, only when read after leaving it.
    let text = profile(&graph, "FOR x IN [1] WITH ORDINALITY i RETURN x, i");
    assert!(!text.contains("as(\""), "{text}");
    let text = profile(
        &graph,
        "FOR x IN ['marko'] WITH ORDINALITY i MATCH (p:person {name: x}) RETURN i",
    );
    assert!(text.contains("as(\"#f0\")"), "{text}");
    assert!(
        text.contains("gql:for(\"x IN ['marko'] WITH ORDINALITY i\")"),
        "{text}"
    );
    // UNWIND renders as the GQL form.
    let text = profile(&graph, "UNWIND [1] AS x RETURN x");
    assert!(text.contains("gql:for(\"x IN [1]\")"), "{text}");
    // A FOR after a pattern keeps the pattern's labels only where read.
    let text = profile(&graph, "MATCH (p:person) FOR x IN [p.name] RETURN x");
    assert_eq!(
        steps(&text),
        [
            "v(labels: [\"person\"])",
            "gql:for(\"x IN [p.name]\")",
            "gql:project(\"x\")"
        ]
    );
}

#[test]
fn stage_2a_queries_never_panic() {
    let graph = modern();
    let heads = [
        "",
        "MATCH (a:person)",
        "MATCH (a)-[e]->(b)",
        "OPTIONAL MATCH (a)",
        "LET x = 1",
        "WITH 1 AS x",
        "MATCH p = (a)-->(b)",
        "MATCH (a) WITH a, count(*) AS c",
    ];
    let fors = [
        "FOR x IN [1, 2]",
        "FOR a IN [1]",
        "UNWIND $p AS e",
        "FOR x IN a.l WITH ORDINALITY i",
        "FOR x IN [x] WITH OFFSET y",
        "FOR n IN nodes(p)",
        "FOR n IN [a, b] WITH ORDINALITY i",
        "UNWIND [e] AS r",
    ];
    let tails = [
        "RETURN *",
        "RETURN x",
        "MATCH (x)-->(c) RETURN c",
        "MATCH (n)-->(c) RETURN n, c",
        "MATCH (i) RETURN i",
        "MATCH ()-[r]->() RETURN r",
        "MATCH (n)-[x]->{1,2}() RETURN n",
        "WITH x, count(*) AS c RETURN c",
        "FOR y IN [x] RETURN y",
        "OPTIONAL MATCH (n)-->(m) RETURN m, i",
    ];
    let graph = graph.read();
    for h in heads {
        for f in fors {
            for t in tails {
                let query = format!("{h} {f} {t}");
                let _ = graph
                    .traversal()
                    .gql_with(
                        &query,
                        [("p", ElementProperty::Array(vec![ElementProperty::from(1)]))],
                    )
                    .to_list();
            }
        }
    }
}
