//! GQL's Cypher-compatible regular-expression match `s =~ pattern`: whole-string semantics
//! (Java's `String.matches`, as Neo4j), the Rust `regex` dialect of `TextP.regex`, three-valued
//! logic, `$param` and per-row patterns, the invalid-pattern error 2201B and its phase, the
//! pushdown to `has(key, TextP.regex("^(?:pattern)$"))`, precedence, and the `~` edge error
//! that stays.
#![cfg(feature = "gql")]

use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;

fn modern() -> TraversalGraph {
    TraversalGraph::tinkerpop_modern()
}

fn rows_with(
    graph: &TraversalGraph,
    query: &str,
    params: &[(&str, ElementProperty)],
) -> Vec<ElementProperty> {
    graph
        .traversal()
        .gql_with(query, params.iter().map(|(k, v)| (*k, v.clone())))
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

/// A value as comparable text: `null`, `'text'`, `true` / `false`, other values by `Debug`.
fn norm(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => format!("'{s}'"),
        ElementProperty::Boolean(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

/// Every row of `query` as `a|b|..` over its columns `keys`, sorted.
fn table_with(
    graph: &TraversalGraph,
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

fn table(graph: &TraversalGraph, query: &str, keys: &[&str]) -> Vec<String> {
    table_with(graph, query, keys, &[])
}

/// The names `query` returns in its column `n`, sorted.
fn names(query: &str) -> Vec<String> {
    table(&modern(), query, &["n"])
}

/// The error of `query`.
fn error_of(
    graph: &TraversalGraph,
    query: &str,
    params: &[(&str, ElementProperty)],
) -> Box<TraverserError> {
    match graph
        .traversal()
        .gql_with(query, params.iter().map(|(k, v)| (*k, v.clone())))
        .to_list()
    {
        Ok(rows) => panic!("{query} gave {rows:?}"),
        Err(error) => error,
    }
}

/// The GQLSTATUS, kind and message of the GQL root cause of `error`.
fn gql_cause(error: &TraverserError) -> (String, GqlErrorKind, String) {
    match error.root_cause() {
        TraverserError::Gql {
            kind,
            gqlstatus,
            message,
            ..
        } => (gqlstatus.to_string(), *kind, message.clone()),
        other => panic!("not a GQL error: {other}"),
    }
}

/// The steps of the profile of `query`, without the metrics columns.
fn steps(graph: &TraversalGraph, query: &str, params: &[(&str, ElementProperty)]) -> Vec<String> {
    let profile = graph
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

fn s(text: &str) -> ElementProperty {
    ElementProperty::from(text)
}

#[test]
fn twin_of_text_p_regex_with_the_anchored_pattern() {
    let graph = modern();
    for pattern in [
        "m.*",
        ".*a.*",
        "(?i)J.*",
        "[jv].*|p.*",
        "ar",
        "mark|jos",
        "",
    ] {
        let query = format!("MATCH (n:person) WHERE n.name =~ '{pattern}' RETURN n.name AS n");
        let gql = table(&graph, &query, &["n"]);
        let mut gremlin: Vec<String> = graph
            .traversal()
            .v(None)
            .has_label("person")
            .has_p(
                "name",
                P::regex(&format!("^(?:{pattern})$")).expect("valid regex"),
            )
            .values("name")
            .to_list()
            .unwrap()
            .iter()
            .map(norm)
            .collect();
        gremlin.sort();
        assert_eq!(gql, gremlin, "{query}");
    }
}

#[test]
fn the_pattern_matches_the_whole_string() {
    assert_eq!(
        names("MATCH (n:person) WHERE n.name =~ 'm.*' RETURN n.name AS n"),
        ["'marko'"]
    );
    // A part of the string is not enough (TextP.regex alone would find it).
    assert!(names("MATCH (n:person) WHERE n.name =~ 'ar' RETURN n.name AS n").is_empty());
    assert_eq!(
        names("MATCH (n:person) WHERE n.name =~ '.*ar.*' RETURN n.name AS n"),
        ["'marko'"]
    );
    // An alternation is anchored as a whole: neither `marko` nor `josh` matches.
    assert!(names("MATCH (n:person) WHERE n.name =~ 'mark|jos' RETURN n.name AS n").is_empty());
    assert_eq!(
        names("MATCH (n:person) WHERE n.name =~ 'marko|jos.' RETURN n.name AS n"),
        ["'josh'", "'marko'"]
    );
    // `$` matches only at the very end (no trailing line break allowed, as Java's `matches`).
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "RETURN 'ab\\n' =~ 'ab' AS a, 'ab' =~ 'ab' AS b",
            &["a", "b"]
        ),
        ["false|true"]
    );
}

#[test]
fn rust_regex_dialect_and_inline_flags() {
    assert_eq!(
        names("MATCH (n:person) WHERE n.name =~ '(?i)MARKO' RETURN n.name AS n"),
        ["'marko'"]
    );
    assert_eq!(
        names("MATCH (n:person) WHERE n.name =~ '\\\\w{4}' RETURN n.name AS n"),
        ["'josh'"]
    );
    // `(?m)` inside the pattern does not loosen the outer anchors.
    let graph = modern();
    assert_eq!(
        table(&graph, "RETURN 'a\\nb' =~ '(?m)a$' AS a", &["a"]),
        ["false"]
    );
    // A pattern ending in an `(?x)` comment still matches as a whole.
    assert_eq!(
        table(
            &graph,
            "RETURN 'ab' =~ '(?x) a b # letters' AS a, 'abc' =~ '(?x) a b # letters' AS b",
            &["a", "b"]
        ),
        ["true|false"]
    );
    // A pattern whose groups are unbalanced cannot escape the anchors: it is invalid.
    let (status, ..) = gql_cause(&error_of(&graph, "RETURN 'b' =~ 'a)|(b' AS x", &[]));
    assert_eq!(status, "2201B");
}

#[test]
fn null_and_non_string_operands_give_null() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "RETURN NULL =~ 'a' AS a, 'a' =~ NULL AS b, 1 =~ '1' AS c, '1' =~ 1 AS d, \
             [1] =~ '.*' AS e",
            &["a", "b", "c", "d", "e"]
        ),
        ["null|null|null|null|null"]
    );
    // A missing property is NULL: the row is filtered out, and NOT keeps it out too.
    assert!(names("MATCH (n:software) WHERE n.age =~ '.*' RETURN n.name AS n").is_empty());
    assert!(names("MATCH (n:software) WHERE NOT n.age =~ '.*' RETURN n.name AS n").is_empty());
    // A number property is no string.
    assert!(names("MATCH (n:person) WHERE n.age =~ '29' RETURN n.name AS n").is_empty());
}

#[test]
fn not_negates_and_the_result_is_a_boolean_value() {
    assert_eq!(
        names("MATCH (n:person) WHERE NOT n.name =~ 'm.*' RETURN n.name AS n"),
        ["'josh'", "'peter'", "'vadas'"]
    );
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "MATCH (n:person) RETURN n.name AS n, n.name =~ '[jv].*' AS m",
            &["n", "m"]
        ),
        [
            "'josh'|true",
            "'marko'|false",
            "'peter'|false",
            "'vadas'|true"
        ]
    );
    // It binds like STARTS WITH: tighter than `=` and AND / OR.
    assert_eq!(
        table(
            &graph,
            "RETURN 'a' =~ 'a' = true AS a, 'a' =~ 'b' OR 'b' =~ 'b' AS b, \
             'ab' =~ 'a' + 'b' AS c",
            &["a", "b", "c"]
        ),
        ["true|true|true"]
    );
}

#[test]
fn a_parameter_pattern_is_pushed_down_like_a_literal() {
    let graph = modern();
    let query = "MATCH (n:person) WHERE n.name =~ $p RETURN n.name AS n";
    assert_eq!(
        table_with(&graph, query, &["n"], &[("p", s("v.*|j.*"))]),
        ["'josh'", "'vadas'"]
    );
    assert_eq!(
        steps(&graph, query, &[("p", s("v.*"))]),
        [
            "v(labels: [\"person\"])",
            "has(\"name\", TextP.regex(\"^(?:v.*)$\"))",
            "gql:project(\"n\")"
        ]
    );
    assert_eq!(
        steps(
            &graph,
            "MATCH (n:person) WHERE n.name =~ 'm.*' RETURN n.name AS n",
            &[]
        ),
        [
            "v(labels: [\"person\"])",
            "has(\"name\", TextP.regex(\"^(?:m.*)$\"))",
            "gql:project(\"n\")"
        ]
    );
    // A non-string parameter is not pushed; it gives NULL, so no row.
    assert!(table_with(&graph, query, &["n"], &[("p", ElementProperty::from(1))]).is_empty());
    let plan = steps(&graph, query, &[("p", ElementProperty::from(1))]);
    assert!(
        plan.iter().any(|step| step.starts_with("gql:filter")),
        "{plan:?}"
    );
}

#[test]
fn a_pattern_computed_per_row() {
    let graph = modern();
    // A pattern from a property of the same row: every name matches itself.
    assert_eq!(
        table(
            &graph,
            "MATCH (n:person) WHERE n.name =~ n.name RETURN n.name AS n",
            &["n"]
        ),
        ["'josh'", "'marko'", "'peter'", "'vadas'"]
    );
    // Patterns from a column, alternating per row (the cache is replaced and found again).
    assert_eq!(
        table(
            &graph,
            "FOR p IN ['m.*', 'j.*', 'm.*', 'x', 'j.*'] MATCH (n:person) WHERE n.name =~ p \
             RETURN p, n.name AS n",
            &["p", "n"]
        ),
        [
            "'j.*'|'josh'",
            "'j.*'|'josh'",
            "'m.*'|'marko'",
            "'m.*'|'marko'"
        ]
    );
    // A pattern from another element's property.
    assert_eq!(
        table(
            &graph,
            "MATCH (a:person {name: 'marko'}), (b:person) WHERE b.name =~ (a.name + '|j.*') \
             RETURN b.name AS n",
            &["n"]
        ),
        ["'josh'", "'marko'"]
    );
    // An invalid pattern computed per row fails when the row reaches it.
    let error = error_of(
        &graph,
        "MATCH (n:person) RETURN n.name =~ (n.name + '(') AS x",
        &[],
    );
    let (status, kind, message) = gql_cause(&error);
    assert_eq!((status.as_str(), kind), ("2201B", GqlErrorKind::Data));
    assert!(message.contains("invalid regular expression"), "{message}");
    assert!(
        !error.to_string().contains("before the traversal ran"),
        "{error}"
    );
    // No row reaches it on the empty graph: no error.
    let empty = TraversalGraph::new();
    assert!(
        rows_with(
            &empty,
            "MATCH (n:person) RETURN n.name =~ (n.name + '(') AS x",
            &[]
        )
        .is_empty()
    );
}

#[test]
fn an_invalid_literal_pattern_fails_before_the_run() {
    // The empty graph: no row would ever reach the expression.
    let empty = TraversalGraph::new();
    for query in [
        "MATCH (n:person) WHERE n.name =~ 'm(' RETURN n.name AS n",
        "MATCH (n:person) RETURN n.name =~ '[a' AS x",
        "MATCH (n:person) WHERE n.name STARTS WITH 'm' OR n.name =~ '(?<=a)b' RETURN n",
    ] {
        let error = error_of(&empty, query, &[]);
        let (status, kind, message) = gql_cause(&error);
        assert_eq!(
            (status.as_str(), kind),
            ("2201B", GqlErrorKind::Data),
            "{query}"
        );
        assert!(
            message.contains("invalid regular expression"),
            "{query}: {message}"
        );
        assert!(
            error.to_string().contains("before the traversal ran"),
            "{query}: {error}"
        );
        let help = error.help().unwrap_or_default();
        assert!(help.contains("Rust `regex` crate"), "{query}: {help}");
        assert!(help.contains("(?i)"), "{query}: {help}");
        assert!(!help.contains("A row of the data"), "{query}: {help}");
    }
    // The span points at the pattern.
    let error = error_of(
        &empty,
        "MATCH (n:person) WHERE n.name =~ 'm(' RETURN n.name AS n",
        &[],
    );
    assert!(format!("{error}").contains("line 1, column 34"), "{error}");
}

#[test]
fn an_invalid_parameter_pattern_fails_before_the_run_and_writes_nothing() {
    let query = "MATCH (n:person) WHERE n.name =~ $p RETURN n.name AS n";
    let error = error_of(&TraversalGraph::new(), query, &[("p", s("v.*["))]);
    let (status, kind, message) = gql_cause(&error);
    assert_eq!((status.as_str(), kind), ("2201B", GqlErrorKind::Data));
    assert!(message.contains("v.*["), "{message}");
    assert!(
        error.to_string().contains("before the traversal ran"),
        "{error}"
    );
    // A writing query is refused as a whole before its INSERT runs.
    let mut graph = modern();
    let before = graph.vertex_count();
    let error = graph
        .traversal_mut()
        .gql_with(
            "INSERT (:probe) WITH 1 AS one MATCH (n:person) WHERE n.name =~ $p RETURN n.name AS n",
            [("p", s("("))],
        )
        .to_list()
        .err()
        .unwrap_or_else(|| panic!("the query ran"));
    assert_eq!(gql_cause(&error).0, "2201B");
    assert!(
        error.to_string().contains("before the traversal ran"),
        "{error}"
    );
    assert_eq!(graph.vertex_count(), before);
}

#[test]
fn undirected_edges_keep_their_error() {
    let graph = modern();
    for query in [
        "MATCH (a)~[e]~(b) RETURN a",
        "MATCH (a)~(b) RETURN a",
        "MATCH (a) WHERE a.name = ~'x' RETURN a",
    ] {
        let error = error_of(&graph, query, &[]);
        let text = error.to_string();
        assert!(!text.contains("=~"), "{query}: {text}");
        if query.contains("~[") || query.contains(")~(") {
            assert!(text.contains("undirected edges"), "{query}: {text}");
        }
    }
    // `=~` is never read as an undirected edge.
    let error = error_of(&graph, "MATCH (a) WHERE a.name =~ 'm(' RETURN a", &[]);
    assert!(!error.to_string().contains("undirected"), "{error}");
}
