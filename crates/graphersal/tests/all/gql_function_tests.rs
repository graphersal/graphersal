//! GQL stage 1c scalar functions through `gql()` (feature `gql`): one test group per family
//! (conversions, strings, numbers, lists / elements / paths), the functions in every clause
//! (`WHERE`, `LET` / `FILTER`, `WITH`, `ORDER BY`, inside and around aggregates, on `OPTIONAL MATCH`
//! NULLs) and the plan. Differential twins against Gremlin where the semantics agree (rows compared
//! as multisets, design D18); hand-computed rows where they do not (Neo4j-derived values: `round`,
//! `range`, `split` empties, `replace` with `''`, lenient `toX` on bad text, `startNode` under an
//! either-direction edge). The unit tables of every function live beside the family files
//! (`src/gql/functions/*`); this file checks them end to end on real graphs.
#![cfg(feature = "gql")]

use std::sync::Arc;

use graphersal::prelude::*;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// The modern graph plus `word` vertices with UTF-8 names, a padded name, a stored `null` name
/// and no name.
fn words() -> Arc<Graph> {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for (name, rank) in [
        (Some(ElementProperty::from("héllo")), 1),
        (Some(ElementProperty::from("日本語")), 2),
        (Some(ElementProperty::from("a🎉b")), 3),
        (Some(ElementProperty::from("  pad ")), 4),
        (Some(ElementProperty::Null), 5),
        (None, 6),
    ] {
        let mut g = graph.traversal_mut().add_v("word").property("rank", rank);
        if let Some(name) = name {
            g = g.property("name", name);
        }
        g.iterate().unwrap();
    }
    Arc::new(Graph::new(graph))
}

fn rows(graph: &Arc<Graph>, query: &str) -> Vec<ElementProperty> {
    graph
        .read()
        .traversal()
        .gql(query)
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

fn column<'r>(row: &'r ElementProperty, key: &str) -> &'r ElementProperty {
    match row {
        ElementProperty::Object(map) => map
            .get(key)
            .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
        other => panic!("not a row map: {other:?}"),
    }
}

/// A value as comparable text keeping its type: `null`, `'text'`, `i:1`, `f:1.0`, `true`,
/// `[..]`, `{k: v, ..}` (keys sorted), `v<id>`, `e<id>`. With `sets`, list items are sorted
/// (storage order is not part of the contract, design D18).
fn norm(value: &ElementProperty, sets: bool) -> String {
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
            let mut items: Vec<String> = items.iter().map(|v| norm(v, sets)).collect();
            if sets {
                items.sort();
            }
            format!("[{}]", items.join(", "))
        }
        ElementProperty::Object(map) => {
            let mut items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", norm(v, sets)))
                .collect();
            items.sort();
            format!("{{{}}}", items.join(", "))
        }
        ElementProperty::Map(map) => {
            let mut items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", norm(k, sets), norm(v, sets)))
                .map(|s| s.trim_start_matches('\'').replacen("': ", ": ", 1))
                .collect();
            items.sort();
            format!("{{{}}}", items.join(", "))
        }
        ElementProperty::Vertex(v) => format!("v{}", v.id()),
        ElementProperty::Edge(e) => format!("e{}", e.id()),
        other => format!("{other:?}"),
    }
}

/// Every row of `query` as `a|b|..` over `keys`, sorted (a multiset).
fn table(graph: &Arc<Graph>, query: &str, keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = rows(graph, query)
        .iter()
        .map(|row| {
            keys.iter()
                .map(|k| norm(column(row, k), false))
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect();
    out.sort();
    out
}

/// Every row of `query` as `a|b|..` over `keys`, in result order (`ORDER BY`).
fn ordered(graph: &Arc<Graph>, query: &str, keys: &[&str]) -> Vec<String> {
    rows(graph, query)
        .iter()
        .map(|row| {
            keys.iter()
                .map(|k| norm(column(row, k), false))
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

/// The value of `expr` evaluated once (on marko's row), as `norm` text.
fn one(graph: &Arc<Graph>, expr: &str) -> String {
    let query = format!("MATCH (n:person {{name: 'marko'}}) RETURN {expr} AS x");
    let result = rows(graph, &query);
    assert_eq!(result.len(), 1, "{query}");
    norm(column(&result[0], "x"), false)
}

/// The GQLSTATUS of the failure of `expr` evaluated on marko's row.
fn status(graph: &Arc<Graph>, expr: &str) -> String {
    let query = format!("MATCH (n:person {{name: 'marko'}}) RETURN {expr} AS x");
    match graph.read().traversal().gql(&query).to_list() {
        Ok(rows) => panic!("{query}: expected an error, got {rows:?}"),
        Err(error) => match error.root_cause() {
            TraverserError::Gql { gqlstatus, .. } => gqlstatus.clone(),
            other => panic!("{query}: not a GQL error: {other}"),
        },
    }
}

/// Asserts `(expression, expected norm text)` rows.
fn check(graph: &Arc<Graph>, cases: &[(&str, &str)]) {
    for (expr, expected) in cases {
        assert_eq!(one(graph, expr), *expected, "{expr}");
    }
}

/// Asserts `(expression, GQLSTATUS)` rows.
fn check_status(graph: &Arc<Graph>, cases: &[(&str, &str)]) {
    for (expr, expected) in cases {
        assert_eq!(status(graph, expr), *expected, "{expr}");
    }
}

// ----- differential twins against Gremlin (Rhai) -----

#[cfg(feature = "script")]
mod twins {
    use std::sync::Arc;

    use graphersal::prelude::*;

    use super::{column, modern, norm, rows};

    /// The `x` column of the GQL query against the values of the Gremlin twin, as multisets
    /// (`sets`: list items sorted too).
    fn twin(graph: &Arc<Graph>, query: &str, gremlin: &str, sets: bool) {
        let mut left: Vec<String> = rows(graph, query)
            .iter()
            .map(|row| norm(column(row, "x"), sets))
            .collect();
        left.sort();
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, gremlin)
            .unwrap_or_else(|e| panic!("`{gremlin}` failed to evaluate: {e}"));
        let lock = graph.read();
        let mut right: Vec<String> = gts
            .ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{gremlin}: {e}"))
            .iter()
            .map(|v| norm(v, sets))
            .collect();
        right.sort();
        assert!(!left.is_empty(), "{query}: no rows to compare");
        assert_eq!(left, right, "\nGQL:  {query}\ntwin: {gremlin}");
    }

    const PERSONS: &str = "MATCH (p:person) RETURN";
    const PERSON_V: &str = r#"g.V().hasLabel("person")"#;

    #[test]
    fn conversions_match_their_twins() {
        let graph = modern();
        for (expr, steps) in [
            ("toString(p.age)", r#"values("age").as_string()"#),
            ("CAST(p.age AS STRING)", r#"values("age").as_string()"#),
            ("toInteger(p.age)", r#"values("age").as_number(GType.LONG)"#),
            (
                "CAST(p.age AS INTEGER)",
                r#"values("age").as_number(GType.LONG)"#,
            ),
            (
                "toIntegerOrNull(p.age)",
                r#"values("age").as_number(GType.LONG)"#,
            ),
            ("toFloat(p.age)", r#"values("age").as_number(GType.DOUBLE)"#),
            ("toBoolean(p.age)", r#"values("age").as_bool()"#),
            ("toString(p.name)", r#"values("name").as_string()"#),
        ] {
            twin(
                &graph,
                &format!("{PERSONS} {expr} AS x"),
                &format!("{PERSON_V}.{steps}"),
                false,
            );
        }
        twin(
            &graph,
            "MATCH ()-[e]->() RETURN toFloat(e.weight) AS x",
            r#"g.E().values("weight").as_number(GType.DOUBLE)"#,
            false,
        );
    }

    #[test]
    fn string_functions_match_their_twins() {
        let graph = modern();
        for (expr, steps) in [
            // GQL substring(s, start, length); Gremlin substring(start, end).
            (
                "substring(p.name, 1, 2)",
                r#"values("name").substring(1, 3)"#,
            ),
            ("substring(p.name, 1)", r#"values("name").substring(1)"#),
            (
                "replace(p.name, 'a', 'XY')",
                r#"values("name").replace("a", "XY")"#,
            ),
            // No empty pieces on these names (Gremlin drops them, GQL keeps them).
            ("split(p.name, 'a')", r#"values("name").split("a")"#),
            ("reverse(p.name)", r#"values("name").reverse()"#),
            ("trim(p.name)", r#"values("name").trim()"#),
            ("ltrim(p.name)", r#"values("name").l_trim()"#),
            ("rtrim(p.name)", r#"values("name").r_trim()"#),
            ("upper(p.name)", r#"values("name").to_upper()"#),
        ] {
            twin(
                &graph,
                &format!("{PERSONS} {expr} AS x"),
                &format!("{PERSON_V}.{steps}"),
                false,
            );
        }
        // Padded with spaces (GQL trims U+0020, Gremlin any whitespace: equal on spaces).
        let words = super::words();
        for (expr, steps) in [
            ("trim(w.name)", r#"values("name").trim()"#),
            ("ltrim(w.name)", r#"values("name").l_trim()"#),
            ("rtrim(w.name)", r#"values("name").r_trim()"#),
        ] {
            twin(
                &words,
                &format!("MATCH (w:word {{rank: 4}}) RETURN {expr} AS x"),
                &format!(r#"g.V().has("word", "rank", 4).{steps}"#),
                false,
            );
        }
    }

    #[test]
    fn numeric_functions_match_their_twins() {
        let graph = modern();
        // Floats on both sides: Gremlin math() always returns a float.
        for (expr, math) in [
            ("sqrt(p.age)", "sqrt(_)"),
            ("exp(p.age / 10.0)", "exp(_ / 10)"),
            ("ln(p.age)", "log(_)"),
            ("log(p.age)", "log(_)"),
            ("log10(p.age)", "log10(_)"),
            ("abs(-1.5 * p.age)", "abs(-1.5 * _)"),
            ("floor(p.age / 10.0)", "floor(_ / 10)"),
            ("ceil(p.age / 10.0)", "ceil(_ / 10)"),
            ("ceiling(p.age / 10.0)", "ceil(_ / 10)"),
            ("power(p.age, 2.0)", "pow(_, 2)"),
        ] {
            twin(
                &graph,
                &format!("{PERSONS} {expr} AS x"),
                &format!(r#"{PERSON_V}.values("age").math("{math}")"#),
                false,
            );
        }
        for (expr, math) in [
            ("abs(e.weight - 0.6)", "abs(_ - 0.6)"),
            ("floor(e.weight * 3)", "floor(_ * 3)"),
            ("ceil(e.weight * 3)", "ceil(_ * 3)"),
        ] {
            twin(
                &graph,
                &format!("MATCH ()-[e]->() RETURN {expr} AS x"),
                &format!(r#"g.E().values("weight").math("{math}")"#),
                false,
            );
        }
    }

    #[test]
    fn element_and_path_functions_match_their_twins() {
        let graph = modern();
        for (query, gremlin, sets) in [
            (
                "MATCH ()-[e]->() RETURN startNode(e) AS x",
                "g.E().out_v()",
                false,
            ),
            (
                "MATCH ()-[e]->() RETURN endNode(e) AS x",
                "g.E().in_v()",
                false,
            ),
            // Written right to left: the endpoints are still the stored ones.
            (
                "MATCH (b)<-[e:created]-(a) RETURN startNode(e) AS x",
                r#"g.E().hasLabel("created").out_v()"#,
                false,
            ),
            (
                "MATCH (n) RETURN keys(n) AS x",
                "g.V().map(__.properties().key().fold())",
                true,
            ),
            (
                "MATCH ()-[e]->() RETURN keys(e) AS x",
                "g.E().map(__.properties().key().fold())",
                true,
            ),
            (
                "MATCH (n) RETURN properties(n) AS x",
                "g.V().value_map().by(__.unfold())",
                false,
            ),
            (
                "MATCH ()-[e]->() RETURN properties(e) AS x",
                "g.E().value_map().by(__.unfold())",
                false,
            ),
            (
                "MATCH p = (a)-[:knows|created]->{1,2}(b) RETURN nodes(p) AS x",
                r#"g.V().repeat(__.out("knows", "created")).emit().times(2).path()"#,
                false,
            ),
            (
                "MATCH p = (a)-[:created]->(b)<-[:created]-(c) RETURN nodes(p) AS x",
                r#"g.V().out("created").in("created").simple_path().path()"#,
                false,
            ),
        ] {
            twin(&graph, query, gremlin, sets);
        }
    }
}

// ----- hand-computed rows -----

#[test]
fn conversions_strict_cast_and_lenient_to_x() {
    let graph = modern();
    check(
        &graph,
        &[
            // Lenient toX: bad text is NULL; Cypher literals and Java trimming (P0 items 3, 5).
            ("toInteger('abc')", "null"),
            ("toInteger(' 42 ')", "i:42"),
            ("toInteger('1.9')", "i:1"),
            ("toInteger('-1.9')", "i:-1"),
            ("toInteger('1e3')", "i:1000"),
            ("toInteger('0x1F')", "i:31"),
            ("toInteger('0o11')", "i:9"),
            ("toInteger('1_000')", "i:1000"),
            ("toInteger(1.9)", "i:1"),
            ("toInteger(true)", "i:1"),
            ("toInteger(null)", "null"),
            ("toFloat('x')", "null"),
            ("toFloat(' 2.5 ')", "f:2.5"),
            ("toFloat(3)", "f:3.0"),
            ("toBoolean('yes')", "null"),
            ("toBoolean(' TRUE ')", "true"),
            ("toBoolean('False')", "false"),
            ("toBoolean(0)", "false"),
            ("toBoolean(2)", "true"),
            // Q1: a float renders with a decimal point (Gremlin as_string gives "1").
            ("toString(1.0)", "'1.0'"),
            ("toString(1.5e20)", "'1.5E20'"),
            ("toString(true)", "'true'"),
            (
                "toString([1, 'a', true, null, 2.0])",
                "'[1, a, true, null, 2.0]'",
            ),
            ("toString({a: 1, b: 'hi'})", "'{a: 1, b: hi}'"),
            ("toString(n.age)", "'29'"),
            // OrNull: wrong kinds become NULL instead of 22G03.
            ("toFloatOrNull(true)", "null"),
            ("toBooleanOrNull(1.5)", "null"),
            ("toIntegerOrNull('x')", "null"),
            ("toIntegerOrNull([1])", "null"),
            // Strict CAST.
            ("CAST('42' AS INTEGER)", "i:42"),
            ("CAST(' 42 ' AS INTEGER)", "i:42"),
            ("CAST(1.9 AS INTEGER)", "i:1"),
            ("CAST(-1.9 AS INTEGER)", "i:-1"),
            ("CAST('2.5' AS FLOAT)", "f:2.5"),
            ("CAST(1.0 AS STRING)", "'1.0'"),
            ("CAST('true' AS BOOLEAN)", "true"),
            ("CAST('unknown' AS BOOLEAN)", "null"),
            ("CAST(null AS INTEGER)", "null"),
            ("CAST(n.age AS FLOAT)", "f:29.0"),
        ],
    );
    check_status(
        &graph,
        &[
            ("CAST('abc' AS INTEGER)", "22018"),
            ("CAST('0x1F' AS INTEGER)", "22018"),
            ("CAST('yes' AS BOOLEAN)", "22018"),
            ("CAST(true AS INTEGER)", "22G03"),
            ("CAST(1 AS BOOLEAN)", "22G03"),
            ("CAST(1.0e19 AS INTEGER)", "22003"),
            ("toInteger(1.0e19)", "22003"),
            ("toFloat(true)", "22G03"),
            ("toBoolean(1.5)", "22G03"),
            ("toString(n)", "22G03"),
            ("toStringOrNull(n)", "22G03"),
        ],
    );
    // Gremlin's as_number(GType.LONG) refuses "1.9"; GQL toInteger truncates (K5).
    let gremlin = graph
        .read()
        .traversal()
        .inject(vec![ElementProperty::from("1.9")])
        .as_number(GType::LONG)
        .to_list();
    assert!(gremlin.is_err(), "{gremlin:?}");
}

#[test]
fn string_functions_hand_computed() {
    let graph = modern();
    check(
        &graph,
        &[
            // split keeps empties (Neo4j), Gremlin drops them.
            ("split('a,,b,', ',')", "['a', '', 'b', '']"),
            ("split('', ',')", "['']"),
            ("split('', '')", "['']"),
            ("split('abc', '')", "['a', 'b', 'c']"),
            ("split('a--b', ['-', '--'])", "['a', '', 'b']"),
            ("split('a--b', ['--', '-'])", "['a', 'b']"),
            ("split('a,b', [])", "['a,b']"),
            ("split('a,b', [',', null])", "null"),
            // replace with an empty search inserts everywhere (Java), Gremlin leaves it.
            ("replace('abc', '', 'x')", "'xaxbxcx'"),
            ("replace('abc', '', 'x', 2)", "'xaxbc'"),
            ("replace('', 'a', 'b')", "''"),
            ("replace('aaa', 'a', 'b', 0)", "'aaa'"),
            ("replace('aaa', 'a', 'b', 2)", "'bba'"),
            // GQL TRIM removes spaces only (P0 item 11).
            ("trim('\ta ')", "'\ta'"),
            ("TRIM(BOTH 'x' FROM 'xxaxx')", "'a'"),
            ("TRIM(LEADING 'x' FROM 'xxaxx')", "'axx'"),
            ("TRIM(TRAILING FROM '  a  ')", "'  a'"),
            ("btrim('xxyyhelloxyxy', 'xy')", "'hello'"),
            ("ltrim('xxa', 'x')", "'a'"),
            ("rtrim('axx', 'x')", "'a'"),
            ("btrim('abc', '')", "'abc'"),
            ("left('marko', 2)", "'ma'"),
            ("right('marko', 2)", "'ko'"),
            ("left('ab', 10)", "'ab'"),
            ("substring('marko', 10)", "''"),
            ("substring('marko', 3, 10)", "'ko'"),
            ("reverse([1, 2, 3])", "[i:3, i:2, i:1]"),
            ("isEmpty('')", "true"),
            ("isEmpty([1])", "false"),
            ("isEmpty({})", "true"),
            ("nullif(n.age, 29)", "null"),
            ("nullif(n.age, 30)", "i:29"),
            ("left(null, 2)", "null"),
            ("substring('abc', null)", "null"),
        ],
    );
    check_status(
        &graph,
        &[
            ("left('abc', -1)", "22011"),
            ("substring('abc', -1, 1)", "22011"),
            ("substring('abc', 1, -1)", "22011"),
            ("TRIM(BOTH 'xy' FROM 'xxa')", "22027"),
            ("replace('a', 'a', 'b', -1)", "22023"),
            ("left(1, 2)", "22G03"),
            ("isEmpty(1)", "22G03"),
        ],
    );
}

#[test]
fn string_functions_count_characters_on_utf8() {
    let graph = words();
    assert_eq!(
        table(
            &graph,
            "MATCH (w:word) RETURN w.rank AS r, char_length(w.name) AS c, byte_length(w.name) AS b, \
             left(w.name, 2) AS l, right(w.name, 2) AS rt, substring(w.name, 1, 1) AS s, \
             reverse(w.name) AS v, upper(w.name) AS u, trim(w.name) AS t",
            &["r", "c", "b", "l", "rt", "s", "v", "u", "t"]
        ),
        sorted(&[
            "i:1|i:5|i:6|'hé'|'lo'|'é'|'olléh'|'HÉLLO'|'héllo'",
            "i:2|i:3|i:9|'日本'|'本語'|'本'|'語本日'|'日本語'|'日本語'",
            "i:3|i:3|i:6|'a🎉'|'🎉b'|'🎉'|'b🎉a'|'A🎉B'|'a🎉b'",
            "i:4|i:6|i:6|'  '|'d '|' '|' dap  '|'  PAD '|'pad'",
            "i:5|null|null|null|null|null|null|null|null",
            "i:6|null|null|null|null|null|null|null|null",
        ])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (w:word) WHERE w.rank < 4 RETURN w.rank AS r, split(w.name, '') AS s, \
             octet_length(w.name) AS o",
            &["r", "s", "o"]
        ),
        sorted(&[
            "i:1|['h', 'é', 'l', 'l', 'o']|i:6",
            "i:2|['日', '本', '語']|i:9",
            "i:3|['a', '🎉', 'b']|i:6",
        ])
    );
}

#[test]
fn numeric_functions_hand_computed() {
    let graph = modern();
    check(
        &graph,
        &[
            // round: one argument and precision 0 without a mode are Java Math.round (ties to
            // +infinity), the result a float (P0).
            ("round(2.5)", "f:3.0"),
            ("round(-2.5)", "f:-2.0"),
            ("round(-2.5, 0)", "f:-2.0"),
            ("round(-1.5, 0)", "f:-1.0"),
            ("round(2.675, 2)", "f:2.68"),
            ("round(-1.55, 1)", "f:-1.6"),
            ("round(1234)", "f:1234.0"),
            ("round(2.5, 0, 'HALF_EVEN')", "f:2.0"),
            ("round(3.5, 0, 'HALF_EVEN')", "f:4.0"),
            ("round(-2.5, 0, 'HALF_UP')", "f:-3.0"),
            ("round(-2.5, 0, 'HALF_DOWN')", "f:-2.0"),
            ("round(2.1, 0, 'UP')", "f:3.0"),
            ("round(-2.1, 0, 'UP')", "f:-3.0"),
            ("round(2.9, 0, 'DOWN')", "f:2.0"),
            ("round(-2.1, 0, 'CEILING')", "f:-2.0"),
            ("round(-2.1, 0, 'FLOOR')", "f:-3.0"),
            ("round(null)", "null"),
            ("abs(-3)", "i:3"),
            ("abs(-3.5)", "f:3.5"),
            ("floor(3)", "i:3"),
            ("ceil(2.1)", "f:3.0"),
            ("sign(-7)", "i:-1"),
            ("sign(0.5)", "i:1"),
            ("sign(0)", "i:0"),
            ("mod(7, 3)", "i:1"),
            ("mod(-7, 3)", "i:-1"),
            ("power(2, 10)", "f:1024.0"),
            ("log(2, 8)", "f:3.0"),
            ("log10(1000)", "f:3.0"),
            ("sqrt(n.age - 25)", "f:2.0"),
        ],
    );
    check_status(
        &graph,
        &[
            ("round(1234, -2)", "22023"),
            ("round(2.5, 0, 'half_up')", "22023"),
            ("round(2.5, 0, 'UNNECESSARY')", "22023"),
            ("round(2.5, 1.0)", "22G03"),
            ("abs(-9223372036854775807 - 1)", "22003"),
            ("sqrt(-1)", "2201F"),
            ("ln(0)", "2201E"),
            ("log10(-1)", "2201E"),
            ("log(1, 8)", "2201E"),
            ("power(0, -1)", "2201F"),
            ("power(-8, 1.0 / 3)", "2201F"),
            ("exp(1000)", "22003"),
            ("mod(1, 0)", "22012"),
            ("abs('x')", "22G03"),
        ],
    );
}

#[test]
fn list_functions_hand_computed() {
    let graph = modern();
    check(
        &graph,
        &[
            ("range(1, 10, 3)", "[i:1, i:4, i:7, i:10]"),
            ("range(0, 3)", "[i:0, i:1, i:2, i:3]"),
            ("range(5, 1)", "[]"),
            ("range(5, 1, -2)", "[i:5, i:3, i:1]"),
            ("range(3, 3)", "[i:3]"),
            ("head([1, 2])", "i:1"),
            ("head([])", "null"),
            ("last([1, 2])", "i:2"),
            ("last([])", "null"),
            ("tail([1, 2, 3])", "[i:2, i:3]"),
            ("tail([])", "[]"),
            ("trim([1, 2, 3], 1)", "[i:1, i:2]"),
            ("trim([1, 2, 3], 5)", "[]"),
            ("keys({b: 1, a: null})", "['b', 'a']"),
            ("properties({a: 1})", "{a: i:1}"),
            ("head(null)", "null"),
        ],
    );
    check_status(
        &graph,
        &[
            ("range(0, 3, 0)", "22023"),
            ("range(0, 3.5)", "22G03"),
            ("trim([1], -1)", "22023"),
            ("head('abc')", "22G03"),
            ("startNode(n)", "22G03"),
            ("keys(1)", "22G03"),
        ],
    );
    // range reserves its result before building it: the materialization limit, never an OOM.
    let error = graph
        .read()
        .traversal()
        .gql("MATCH (n:person {name: 'marko'}) RETURN range(0, 9223372036854775807) AS x")
        .to_list()
        .unwrap_err();
    assert!(
        error
            .root_cause()
            .to_string()
            .contains("max_materialized_bytes"),
        "{error}"
    );
}

#[test]
fn start_node_is_the_stored_source_whatever_the_pattern_direction() {
    let graph = modern();
    // Under `-[e]-` the pattern does not fix a direction: startNode / endNode read the edge.
    assert_eq!(
        table(
            &graph,
            "MATCH (a {name: 'josh'})-[e]-(b) RETURN b.name AS b, startNode(e).name AS s, \
             endNode(e).name AS t, a IS SOURCE OF e AS src, b IS DESTINATION OF e AS dst, \
             b IS NOT SOURCE OF e AS nsrc",
            &["b", "s", "t", "src", "dst", "nsrc"]
        ),
        sorted(&[
            "'lop'|'josh'|'lop'|true|true|true",
            "'marko'|'marko'|'josh'|false|false|false",
            "'ripple'|'josh'|'ripple'|true|true|true",
        ])
    );
    // Written right to left.
    assert_eq!(
        table(
            &graph,
            "MATCH (s {name: 'lop'})<-[e]-(x) RETURN x.name AS x, startNode(e).name AS s, \
             endNode(e).name AS t",
            &["x", "s", "t"]
        ),
        sorted(&[
            "'josh'|'josh'|'lop'",
            "'marko'|'marko'|'lop'",
            "'peter'|'peter'|'lop'",
        ])
    );
    // Also in WHERE.
    assert_eq!(
        table(
            &graph,
            "MATCH (a)-[e]-(b) WHERE a IS SOURCE OF e AND e.weight >= 1.0 RETURN a.name AS a, \
             b.name AS b",
            &["a", "b"]
        ),
        sorted(&["'josh'|'ripple'", "'marko'|'josh'"])
    );
}

#[test]
fn keys_and_properties_of_elements() {
    let graph = modern();
    // Storage order is not part of the contract: compared as sets.
    let set = |query: &str| -> Vec<String> {
        let mut out: Vec<String> = rows(&graph, query)
            .iter()
            .map(|row| norm(column(row, "x"), true))
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        set("MATCH (n:person {name: 'marko'}) RETURN keys(n) AS x"),
        ["['age', 'name']"]
    );
    assert_eq!(
        set("MATCH (n:software {name: 'lop'}) RETURN properties(n) AS x"),
        ["{lang: 'java', name: 'lop'}"]
    );
    assert_eq!(
        set("MATCH ()-[e:knows]->() RETURN properties(e) AS x"),
        ["{weight: f:0.5}", "{weight: f:1.0}"]
    );
    // A stored null is not a property (words(): rank 5 has `name: null`).
    let words = words();
    let rows = rows(
        &words,
        "MATCH (w:word {rank: 5}) RETURN keys(w) AS k, properties(w) AS p",
    );
    assert_eq!(norm(column(&rows[0], "k"), true), "['rank']");
    assert_eq!(norm(column(&rows[0], "p"), true), "{rank: i:5}");
}

#[test]
fn nodes_and_relationships_of_paths() {
    let graph = modern();
    // Quantified edges (`e<n>`: the edge handles of the modern graph, e0 = marko-knows-vadas).
    assert_eq!(
        table(
            &graph,
            "MATCH p = (a {name: 'marko'})-[:knows|created]->{1,2}(b) RETURN nodes(p) AS n, \
             relationships(p) AS r, size(nodes(p)) AS c",
            &["n", "r", "c"]
        ),
        sorted(&[
            "[v1, v2]|[e0]|i:2",
            "[v1, v3]|[e1]|i:2",
            "[v1, v4]|[e2]|i:2",
            "[v1, v4, v3]|[e2, e3]|i:3",
            "[v1, v4, v5]|[e2, e4]|i:3",
        ])
    );
    // Either direction: the path follows the text, the edges keep their stored direction.
    assert_eq!(
        table(
            &graph,
            "MATCH p = (a {name: 'lop'})-[e]-(b) RETURN nodes(p) AS n, relationships(p) AS r, \
             startNode(head(relationships(p))) AS s",
            &["n", "r", "s"]
        ),
        sorted(&["[v3, v1]|[e1]|v1", "[v3, v4]|[e3]|v4", "[v3, v6]|[e5]|v6",])
    );
    // Zero-length path and a path from an OPTIONAL MATCH that did not match.
    assert_eq!(
        table(
            &graph,
            "MATCH p = (a {name: 'lop'}) RETURN nodes(p) AS n, relationships(p) AS r",
            &["n", "r"]
        ),
        ["[v3]|[]"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (a:person) OPTIONAL MATCH p = (a)-[:created]->(:software {name: 'ripple'}) \
             RETURN a.name AS a, nodes(p) AS n, last(nodes(p)) AS l, relationships(p) AS r",
            &["a", "n", "l", "r"]
        ),
        sorted(&[
            "'josh'|[v4, v5]|v5|[e4]",
            "'marko'|null|null|null",
            "'peter'|null|null|null",
            "'vadas'|null|null|null",
        ])
    );
}

// ----- functions in every clause -----

#[test]
fn functions_in_where_let_with_and_order_by() {
    let graph = modern();
    // WHERE: a function conjunct next to a pushed-down one.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) WHERE toString(p.age) STARTS WITH '3' AND p.age > 0 \
             RETURN p.name AS n",
            &["n"]
        ),
        sorted(&["'josh'", "'peter'"])
    );
    // LET and FILTER.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) LET n = left(p.name, 2), a = round(p.age / 10.0) \
             FILTER n = 'ma' RETURN n, a",
            &["n", "a"]
        ),
        ["'ma'|f:3.0"]
    );
    // WITH projecting (substitution) and WITH aggregating on a function key.
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) WITH p, abs(p.age - 30) AS d RETURN p.name AS n, d ORDER BY d, n",
            &["n", "d"]
        ),
        ["'marko'|i:1", "'josh'|i:2", "'vadas'|i:3", "'peter'|i:5"]
    );
    assert_eq!(
        ordered(
            &graph,
            "MATCH (v) WITH coalesce(v.lang, 'none') AS l, count(*) AS c \
             RETURN upper(l) AS l, c ORDER BY l",
            &["l", "c"]
        ),
        ["'JAVA'|i:2", "'NONE'|i:4"]
    );
    // ORDER BY a function the RETURN does not list.
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN p.name AS n ORDER BY reverse(p.name)",
            &["n"]
        ),
        ["'josh'", "'marko'", "'peter'", "'vadas'"]
    );
    assert_eq!(
        ordered(
            &graph,
            "MATCH (p:person) RETURN p.name AS n ORDER BY char_length(p.name) DESC, n",
            &["n"]
        ),
        ["'marko'", "'peter'", "'vadas'", "'josh'"]
    );
}

#[test]
fn functions_inside_and_around_aggregates() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN round(avg(p.age), 1) AS a, \
             max(char_length(p.name)) AS m, sum(round(p.age / 10.0)) AS s, \
             size(collect(p.name)) AS c",
            &["a", "m", "s", "c"]
        ),
        ["f:30.8|i:5|f:13.0|i:4"]
    );
    // head(collect(x)) after an ORDER BY in WITH keeps the order of the collected rows.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) WITH p ORDER BY p.age DESC RETURN head(collect(p.name)) AS h, \
             last(collect(p.name)) AS l",
            &["h", "l"]
        ),
        ["'peter'|'vadas'"]
    );
    // A function of the grouping key.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person)-[:created]->(s) RETURN upper(p.name) AS n, \
             toString(count(s)) AS c",
            &["n", "c"]
        ),
        sorted(&["'JOSH'|'2'", "'MARKO'|'1'", "'PETER'|'1'"])
    );
}

#[test]
fn functions_on_optional_match_nulls() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "MATCH (s:software) OPTIONAL MATCH (s)<-[e:knows]-(x) RETURN s.name AS s, \
             startNode(e) AS a, keys(e) AS k, toString(e.weight) AS w, upper(x.name) AS u, \
             round(e.weight) AS r, isEmpty(x.name) AS i, coalesce(toInteger(x.age), -1) AS c",
            &["s", "a", "k", "w", "u", "r", "i", "c"]
        ),
        sorted(&[
            "'lop'|null|null|null|null|null|null|i:-1",
            "'ripple'|null|null|null|null|null|null|i:-1",
        ])
    );
}

// ----- plan -----

/// The step column of a profile, one entry per step.
fn steps(profile: &str) -> Vec<String> {
    profile
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            !l.is_empty()
                && !l.starts_with("Traversal Metrics")
                && !l.starts_with("Step ")
                && !l.starts_with('=')
                && !l.starts_with("TOTAL")
                && !l.contains("TOTAL:")
                && !l.starts_with("Optimizer")
        })
        .map(|l| l.split("   ").next().unwrap_or_default().trim().to_string())
        .collect()
}

#[test]
fn a_function_free_query_plans_as_before_and_a_function_conjunct_is_never_pushed_down() {
    let graph = modern();
    assert_eq!(
        steps(&profile(
            &graph,
            "MATCH (p:person) WHERE p.name = 'marko' RETURN p.name AS n"
        )),
        [
            r#"v(labels: ["person"])"#,
            r#"has("name", P.eq("marko"))"#,
            r#"gql:project("n")"#
        ]
    );
    assert_eq!(
        steps(&profile(
            &graph,
            "MATCH (p:person) WHERE upper(p.name) = 'MARKO' AND p.age > 20 RETURN p.name AS n"
        )),
        [
            r#"v(labels: ["person"])"#,
            r#"has("age", P.gt(20))"#,
            r#"gql:filter("upper(p.name) = 'MARKO'")"#,
            r#"gql:project("n")"#
        ]
    );
    // An inline property map with a function value is a filter too, never a has().
    let plan = steps(&profile(
        &graph,
        "MATCH (p:person) WHERE p.name = lower('MARKO') RETURN p.name AS n",
    ));
    assert!(plan.iter().any(|s| s.starts_with("gql:filter")), "{plan:?}");
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) WHERE p.name = lower('MARKO') RETURN p.name AS n",
            &["n"]
        ),
        ["'marko'"]
    );
}

/// The limit error `query` (with `$s`, a long string) fails with under `options`: the name of
/// the resource limit at the root cause. Never a panic, never an allocation of the full result.
fn limit_of(graph: &Arc<Graph>, options: &[(&str, i64)], query: &str) -> String {
    let long = "x".repeat(100_000);
    let lock = graph.read();
    let mut g = lock.traversal();
    for (key, value) in options {
        g = g.with(*key, *value);
    }
    let error = g
        .gql_with(query, [("s", ElementProperty::from(long.as_str()))])
        .to_list()
        .expect_err(query);
    match error.root_cause() {
        TraverserError::ResourceLimitExceeded { limit, .. } => limit.clone(),
        other => panic!("{query}: expected a resource limit, got {other}"),
    }
}

/// Design F6: a function whose result can outgrow its arguments (`range`, `replace`, `split`,
/// `||`, `toString` of a list) asks for its size before it builds anything; a blow-up is the
/// materialization error (the engine's default limit included), or the memory-budget error
/// under a budget, in every clause, never a panic or an out-of-memory abort.
#[test]
fn blow_ups_are_refused_by_the_materialization_limit_and_the_memory_budget() {
    let graph = modern();
    let materialized = "traversal.max_materialized_bytes";
    // Under the default limit (1 GiB native): results of 2^63 items or 10^10 bytes.
    for query in [
        "MATCH (n:person {name: 'marko'}) RETURN range(0, 9223372036854775807) AS x",
        "MATCH (n:person {name: 'marko'}) RETURN size(range(-9223372036854775808, \
         9223372036854775807)) AS x",
        "MATCH (n:person {name: 'marko'}) RETURN replace($s, '', $s) AS x",
        "MATCH (n:person {name: 'marko'}) RETURN size(replace($s, 'x', $s)) AS x",
        "MATCH (n:person {name: 'marko'}) RETURN split(replace($s, 'x', $s), '') AS x",
        "MATCH (n:person {name: 'marko'}) RETURN toString(range(1, 100000000)) AS x",
    ] {
        assert_eq!(limit_of(&graph, &[], query), materialized, "{query}");
    }
    // Under a lower limit: every growing function, in every clause and inside aggregates.
    let low = [(materialized, 100_000)];
    for query in [
        "MATCH (n:person {name: 'marko'}) RETURN split($s, '') AS x",
        "MATCH (n:person {name: 'marko'}) RETURN replace(n.name, '', $s, 9223372036854775807) AS x",
        "MATCH (n:person {name: 'marko'}) RETURN $s || $s AS x",
        "MATCH (n:person {name: 'marko'}) RETURN $s + $s AS x",
        "MATCH (n:person {name: 'marko'}) RETURN toString(split($s, '')) AS x",
        "MATCH (n) WHERE size(split($s, '')) > 0 RETURN n",
        "MATCH (n) LET parts = split($s, '') RETURN size(parts) AS x",
        "MATCH (n) WITH n, replace($s, 'x', 'yy') AS t RETURN size(t) AS x",
        "MATCH (n) RETURN n.name AS name ORDER BY size(split($s, ''))",
        "MATCH (n) RETURN size(split($s, '')) AS k, count(*) AS c",
        "MATCH (n) RETURN count(split($s, '')) AS c",
        "MATCH (n) RETURN max(size(range(1, 1000000))) AS c",
        "MATCH (n) RETURN DISTINCT size(range(1, 1000000)) AS c",
        "MATCH (n) OPTIONAL MATCH (n)-[:knows]->(f) RETURN size(split($s || f.name, '')) AS c",
    ] {
        assert_eq!(limit_of(&graph, &low, query), materialized, "{query}");
    }
    // Under a memory budget (the engine's estimate in this test binary: the graph's footprint
    // plus what the query holds and is about to build) with the default materialization limit.
    let footprint = graph.read().memory_usage().unwrap().total_bytes();
    let budget = [("memory.limit", (footprint + 1_000_000) as i64)];
    for query in [
        "MATCH (n:person {name: 'marko'}) RETURN size(range(1, 10000000)) AS x",
        "MATCH (n:person {name: 'marko'}) RETURN size(replace($s, 'x', 'yyyyyyyyyyyyyyyyyyyy')) AS x",
        "MATCH (n:person {name: 'marko'}) RETURN size(split($s, '')) AS x",
    ] {
        assert_eq!(limit_of(&graph, &budget, query), "memory.limit", "{query}");
    }
    // The same queries with a small enough input run under the same limits.
    let lock = graph.read();
    let rows = lock
        .traversal()
        .with(materialized, 100_000)
        .with("memory.limit", (footprint + 1_000_000) as i64)
        .gql(
            "MATCH (n:person {name: 'marko'}) RETURN size(range(1, 100)) AS r, \
             size(split(n.name, '')) AS s, replace(n.name, '', '-') AS t",
        )
        .to_list()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(column(&rows[0], "r"), &ElementProperty::from(100));
    assert_eq!(column(&rows[0], "s"), &ElementProperty::from(5));
    assert_eq!(column(&rows[0], "t"), &ElementProperty::from("-m-a-r-k-o-"));
}
