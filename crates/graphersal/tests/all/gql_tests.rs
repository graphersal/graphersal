//! `GraphTraversalSource::gql()` (feature `gql`): GQL read queries lowered into the ordinary
//! pipeline. Differential tests pair each query with a hand-written Gremlin twin of the same
//! NULL semantics and compare the rows as multisets, or in order when the query has `ORDER BY`
//! (design D18); the rest checks the row shape, parameters, errors before the run and the
//! lowered plan in `.profile()`.
#![cfg(feature = "gql")]

use graphersal::catalog::PropertyIndexDefinition;
use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;
use graphersal::storage::ElementKind;

/// The modern graph plus persons with a stored `null` age, a float age, a string age, no age,
/// and a second label.
fn fixture() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let mut add = |labels: Vec<&str>, name: &str, age: Option<ElementProperty>| {
        let mut g = graph.traversal_mut().add_v(labels).property("name", name);
        if let Some(age) = age {
            g = g.property("age", age);
        }
        g.iterate().unwrap();
    };
    add(vec!["person"], "nullage", Some(ElementProperty::Null));
    add(
        vec!["person"],
        "floatage",
        Some(ElementProperty::from(32.0)),
    );
    add(vec!["person"], "strage", Some(ElementProperty::from("x")));
    add(vec!["person"], "noage", None);
    add(
        vec!["person", "employee"],
        "emma",
        Some(ElementProperty::from(41)),
    );
    graph
}

fn rows(graph: &TraversalGraph, query: &str) -> Vec<ElementProperty> {
    graph
        .traversal()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

fn rows_with(
    graph: &TraversalGraph,
    query: &str,
    params: Vec<(&str, ElementProperty)>,
) -> TraverserResult<Vec<ElementProperty>> {
    graph.traversal().gql_with(query, params).to_list()
}

/// The keys of a row map in their order.
fn keys(row: &ElementProperty) -> Vec<String> {
    match row {
        ElementProperty::Map(map) => map
            .keys()
            .map(|k| k.as_string().unwrap_or_default().to_string())
            .collect(),
        ElementProperty::Object(map) => map.keys().map(|k| k.to_string()).collect(),
        other => panic!("not a row map: {other:?}"),
    }
}

/// The value of column `key` of a row map.
fn column(row: &ElementProperty, key: &str) -> ElementProperty {
    match row {
        ElementProperty::Map(map) => map.get_str(key).cloned(),
        ElementProperty::Object(map) => map.get(key).cloned(),
        other => panic!("not a row map: {other:?}"),
    }
    .unwrap_or_else(|| panic!("no column {key} in {row:?}"))
}

/// The column `key` of every row, as text (`null` for NULL).
fn texts(rows: &[ElementProperty], key: &str) -> Vec<String> {
    rows.iter()
        .map(|row| match column(row, key) {
            ElementProperty::Null => "null".to_string(),
            ElementProperty::String(s) => s.to_string(),
            ElementProperty::Number(n) => match n.as_i64() {
                Some(i) => i.to_string(),
                None => n.as_f64().unwrap_or_default().to_string(),
            },
            other => format!("{other:?}"),
        })
        .collect()
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

/// The GQL error of a failed traversal: kind, GQLSTATUS, message.
fn gql_error(result: TraverserResult<Vec<ElementProperty>>) -> (GqlErrorKind, String, String) {
    let error = match result {
        Ok(rows) => panic!("expected an error, got {rows:?}"),
        Err(error) => error,
    };
    match error.root_cause() {
        TraverserError::Gql {
            kind,
            gqlstatus,
            message,
            ..
        } => (*kind, gqlstatus.clone(), message.clone()),
        other => panic!("not a GQL error: {other}"),
    }
}

/// Whether a line of the profile starts with the step `prefix`.
fn has_step(profile: &str, prefix: &str) -> bool {
    profile
        .lines()
        .any(|line| line.trim_start().starts_with(prefix))
}

fn profile(graph: &TraversalGraph, query: &str) -> String {
    graph
        .traversal()
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .to_string()
}

// ----- row shape -----

#[test]
fn columns_keep_written_order_and_null_columns() {
    let graph = fixture();
    let result = rows(
        &graph,
        "MATCH (p:person) WHERE p.name = 'noage' RETURN p.name AS name, p.age AS age, 1 + 2 AS three",
    );
    assert_eq!(result.len(), 1);
    assert_eq!(keys(&result[0]), ["name", "age", "three"]);
    assert_eq!(column(&result[0], "age"), ElementProperty::Null);
    assert_eq!(column(&result[0], "three"), ElementProperty::from(3));
    // Reversed order, unaliased names are the expression text.
    let result = rows(
        &graph,
        "MATCH (p:person {name: 'marko'}) RETURN p.age, p.name",
    );
    assert_eq!(keys(&result[0]), ["p.age", "p.name"]);
}

#[test]
fn return_star_lists_variables_in_first_appearance_order() {
    let graph = TraversalGraph::tinkerpop_modern();
    let result = rows(
        &graph,
        "MATCH (s:software {name: 'lop'}), (p:person {name: 'marko'}) RETURN *",
    );
    assert_eq!(result.len(), 1);
    assert_eq!(keys(&result[0]), ["s", "p"]);
    assert!(matches!(
        column(&result[0], "p"),
        ElementProperty::Vertex(_)
    ));
    // `RETURN *, expr`: the variables first.
    let result = rows(
        &graph,
        "MATCH (p:person {name: 'marko'}) RETURN *, p.age AS age",
    );
    assert_eq!(keys(&result[0]), ["p", "age"]);
}

#[test]
fn gremlin_steps_follow_a_returned_element() {
    let graph = TraversalGraph::tinkerpop_modern();
    let created = graph
        .traversal()
        .gql("MATCH (p:person) WHERE p.name IN ['marko', 'josh'] RETURN p")
        .select("p")
        .out("created")
        .values("name")
        .to_list()
        .unwrap();
    let mut names: Vec<String> = created
        .iter()
        .map(|v| v.as_string().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["lop", "lop", "ripple"]);
    let limited = graph
        .traversal()
        .gql("MATCH (p:person) RETURN p.name AS name")
        .limit(2)
        .to_list()
        .unwrap();
    assert_eq!(limited.len(), 2);
}

// ----- WHERE and NULL semantics (expected rows written out) -----

fn names(graph: &TraversalGraph, condition: &str) -> Vec<String> {
    let query = format!("MATCH (p:person) WHERE {condition} RETURN p.name AS n");
    sorted(texts(&rows(graph, &query), "n"))
}

#[test]
fn where_follows_three_valued_logic() {
    let graph = fixture();
    let check = |condition: &str, expected: &[&str]| {
        assert_eq!(names(&graph, condition), expected, "WHERE {condition}");
    };
    // A missing property, a stored null and a string age are NULL or incomparable.
    check("p.age > 30", &["emma", "floatage", "josh", "peter"]);
    check("p.age = 32", &["floatage", "josh"]);
    check("p.age = 32.0", &["floatage", "josh"]);
    check("NOT (p.age > 30)", &["marko", "vadas"]);
    check(
        "p.age <> 29",
        &["emma", "floatage", "josh", "peter", "vadas"],
    );
    check("p.age = 'x'", &["strage"]);
    check("p.age IS NULL", &["noage", "nullage"]);
    check(
        "p.age IS NOT NULL",
        &[
            "emma", "floatage", "josh", "marko", "peter", "strage", "vadas",
        ],
    );
    check("p.age = NULL", &[]);
    check("p.age IN [27, NULL, 35]", &["peter", "vadas"]);
    check("NOT (p.age IN [27, NULL])", &[]);
    check("p.age < 28 OR p.name = 'josh'", &["josh", "vadas"]);
    check(
        "p.age IS NULL OR p.age < 28",
        &["noage", "nullage", "vadas"],
    );
    check("p.name STARTS WITH 'ma'", &["marko"]);
    check(
        "p.name ENDS WITH 'age'",
        &["floatage", "noage", "nullage", "strage"],
    );
    check("p.name CONTAINS 'ete'", &["peter"]);
    check("p.age STARTS WITH 'x'", &["strage"]);
    check("1 = 2", &[]);
    check("p.age > 30 AND p.age < 40", &["floatage", "josh", "peter"]);
}

#[test]
fn label_expressions() {
    let graph = fixture();
    let labelled = |pattern: &str| {
        let query = format!("MATCH (n{pattern}) RETURN n.name AS n");
        sorted(texts(&rows(&graph, &query), "n"))
    };
    assert_eq!(labelled(":software"), ["lop", "ripple"]);
    assert_eq!(labelled(":person&employee"), ["emma"]);
    assert_eq!(labelled(":person:employee"), ["emma"]);
    assert_eq!(labelled(":employee|software"), ["emma", "lop", "ripple"]);
    assert_eq!(
        labelled(":person&!employee").len(),
        8,
        "every person but emma"
    );
    assert_eq!(labelled(":!person"), ["lop", "ripple"]);
    assert_eq!(labelled(":%").len(), 11);
    assert_eq!(labelled(":(person|software)&!employee").len(), 10);
    assert_eq!(labelled(":software&person"), Vec::<String>::new());
    assert_eq!(labelled(" IS software"), ["lop", "ripple"]);
}

#[test]
fn several_patterns_are_a_cartesian_product() {
    let graph = TraversalGraph::tinkerpop_modern();
    let result = rows(
        &graph,
        "MATCH (a:person), (s:software) WHERE a.age > 30 RETURN a.name AS a, s.name AS s",
    );
    let pairs: Vec<String> = sorted(
        result
            .iter()
            .map(|row| {
                format!(
                    "{}-{}",
                    texts(&[row.clone()], "a")[0],
                    texts(&[row.clone()], "s")[0]
                )
            })
            .collect(),
    );
    assert_eq!(
        pairs,
        ["josh-lop", "josh-ripple", "peter-lop", "peter-ripple"]
    );
    // A condition over both bindings.
    let result = rows(
        &graph,
        "MATCH (a:person) MATCH (b:person) WHERE a.age + 3 = b.age RETURN a.name AS a, b.name AS b",
    );
    let pairs: Vec<String> = sorted(
        texts(&result, "a")
            .iter()
            .zip(texts(&result, "b"))
            .map(|(a, b)| format!("{a}-{b}"))
            .collect(),
    );
    assert_eq!(pairs, ["josh-peter", "marko-josh"]);
}

// ----- RETURN DISTINCT, ORDER BY, OFFSET, LIMIT -----

#[test]
fn distinct_rows() {
    let graph = TraversalGraph::tinkerpop_modern();
    let result = rows(&graph, "MATCH (s:software) RETURN DISTINCT s.lang AS lang");
    assert_eq!(texts(&result, "lang"), ["java"]);
    let result = rows(
        &graph,
        "MATCH (n) RETURN DISTINCT n.lang AS lang ORDER BY lang",
    );
    assert_eq!(texts(&result, "lang"), ["java", "null"]);
}

#[test]
fn order_by_puts_null_last_ascending_and_first_descending() {
    let graph = fixture();
    let query =
        "MATCH (p:person) WHERE p.name <> 'strage' RETURN p.name AS n, p.age AS a ORDER BY a, n";
    assert_eq!(
        texts(&rows(&graph, query), "n"),
        [
            "vadas", "marko", "floatage", "josh", "peter", "emma", "noage", "nullage"
        ]
    );
    let query =
        "MATCH (p:person) WHERE p.name <> 'strage' RETURN p.name AS n ORDER BY p.age DESC, n";
    assert_eq!(
        texts(&rows(&graph, query), "n"),
        [
            "noage", "nullage", "emma", "peter", "floatage", "josh", "marko", "vadas"
        ]
    );
    // Types sort in one fixed order: strings before numbers.
    let query =
        "MATCH (p:person) WHERE p.age IS NOT NULL RETURN p.name AS n ORDER BY p.age LIMIT 2";
    assert_eq!(texts(&rows(&graph, query), "n"), ["strage", "vadas"]);
    // A key that is the text of a returned column, and an expression over an alias.
    let query = "MATCH (p:person) WHERE p.age > 0 RETURN p.name, p.age AS a ORDER BY p.name DESC";
    assert_eq!(texts(&rows(&graph, query), "p.name")[0], "vadas");
    let query =
        "MATCH (p:person) WHERE p.age > 0 RETURN p.name AS n, p.age AS a ORDER BY 0 - a LIMIT 1";
    assert_eq!(texts(&rows(&graph, query), "n"), ["emma"]);
}

#[test]
fn offset_and_limit_with_literals_and_parameters() {
    let graph = TraversalGraph::tinkerpop_modern();
    let all = "MATCH (p:person) RETURN p.name AS n ORDER BY n";
    assert_eq!(
        texts(&rows(&graph, all), "n"),
        ["josh", "marko", "peter", "vadas"]
    );
    let paged = rows(&graph, &format!("{all} OFFSET 1 LIMIT 2"));
    assert_eq!(texts(&paged, "n"), ["marko", "peter"]);
    let skipped = rows(&graph, &format!("{all} SKIP 3"));
    assert_eq!(texts(&skipped, "n"), ["vadas"]);
    let none = rows(&graph, &format!("{all} LIMIT 0"));
    assert!(none.is_empty());
    let paged = rows_with(
        &graph,
        &format!("{all} OFFSET $o LIMIT $l"),
        vec![("o", 2.into()), ("l", 5.into())],
    )
    .unwrap();
    assert_eq!(texts(&paged, "n"), ["peter", "vadas"]);
}

#[test]
fn invalid_offset_and_limit_parameters_fail_before_the_run() {
    let empty = TraversalGraph::new();
    let query = "MATCH (p) RETURN p LIMIT $l";
    for (value, status) in [
        (ElementProperty::from(-1), "22G02"),
        (ElementProperty::from(1.5), "22G03"),
        (ElementProperty::from("3"), "22G03"),
        (ElementProperty::Null, "22G03"),
    ] {
        let (kind, gqlstatus, message) =
            gql_error(rows_with(&empty, query, vec![("l", value.clone())]));
        assert_eq!(kind, GqlErrorKind::Semantic, "{value:?}");
        assert_eq!(gqlstatus, status, "{value:?}");
        assert!(message.contains("LIMIT"), "{message}");
    }
    let (_, gqlstatus, _) = gql_error(rows_with(
        &empty,
        "MATCH (p) RETURN p OFFSET $o",
        vec![("o", (-5).into())],
    ));
    assert_eq!(gqlstatus, "22G02");
}

// ----- parameters -----

#[test]
fn parameters_are_values() {
    let graph = fixture();
    let run = |query: &str, params: Vec<(&str, ElementProperty)>| {
        sorted(texts(&rows_with(&graph, query, params).unwrap(), "n"))
    };
    let query = "MATCH (p:person {name: $name}) RETURN p.name AS n";
    assert_eq!(run(query, vec![("name", "josh".into())]), ["josh"]);
    // A parameter that looks like query text is a value.
    assert!(run(query, vec![("name", "x' OR 1=1 --".into())]).is_empty());
    // A NULL parameter never matches.
    assert!(run(query, vec![("name", ElementProperty::Null)]).is_empty());
    let query = "MATCH (p:person) WHERE p.age >= $min RETURN p.name AS n";
    assert_eq!(
        run(query, vec![("min", 32.into())]),
        ["emma", "floatage", "josh", "peter"]
    );
    // A list for IN (its NULL dropped), a list where a scalar is expected (no row).
    let list = ElementProperty::Array(vec![27.into(), ElementProperty::Null, 35.into()]);
    let query = "MATCH (p:person) WHERE p.age IN $ages RETURN p.name AS n";
    assert_eq!(run(query, vec![("ages", list.clone())]), ["peter", "vadas"]);
    let query = "MATCH (p:person) WHERE p.age = $ages RETURN p.name AS n";
    assert!(run(query, vec![("ages", list)]).is_empty());
    let query = "MATCH (p:person) WHERE p.name STARTS WITH $prefix RETURN p.name AS n";
    assert_eq!(run(query, vec![("prefix", "ma".into())]), ["marko"]);
    assert!(run(query, vec![("prefix", 3.into())]).is_empty());
    // An anonymous node: its inline property is pushed, or checked by gql:filter when the
    // value does not fit `has()`.
    let anonymous = "MATCH (:person {age: $a}) RETURN 'row' AS n";
    assert_eq!(run(anonymous, vec![("a", 29.into())]), ["row"]);
    let list = ElementProperty::Array(vec![29.into()]);
    assert!(run(anonymous, vec![("a", list)]).is_empty());
    let text = graph
        .traversal()
        .gql_with(anonymous, [("a", ElementProperty::Array(vec![29.into()]))])
        .profile()
        .unwrap()
        .to_string();
    assert!(text.contains(r#"gql:filter("age: $a")"#), "{text}");
    // Unused parameters are ignored.
    let query = "MATCH (p:person {name: 'josh'}) RETURN p.name AS n";
    assert_eq!(run(query, vec![("unused", 1.into())]), ["josh"]);
}

// ----- errors before the run -----

#[test]
fn errors_fail_before_the_run_on_an_empty_graph() {
    let empty = TraversalGraph::new();
    let (kind, gqlstatus, _) = gql_error(empty.traversal().gql("MATCH (p) RETRUN p").to_list());
    assert_eq!((kind, gqlstatus.as_str()), (GqlErrorKind::Syntax, "42001"));
    let (kind, _, message) = gql_error(empty.traversal().gql("MATCH (p) RETURN q").to_list());
    assert_eq!(kind, GqlErrorKind::Semantic);
    assert!(message.contains('q'), "{message}");
    let (kind, _, message) = gql_error(
        empty
            .traversal()
            .gql("MATCH (p {name: $name}) RETURN p")
            .to_list(),
    );
    assert_eq!(kind, GqlErrorKind::Semantic);
    assert!(message.contains("name"), "{message}");
    // An evaluator error found while compiling: an unknown function.
    let (kind, _, _) = gql_error(
        empty
            .traversal()
            .gql("MATCH (p) RETURN nosuch(p)")
            .to_list(),
    );
    assert_eq!(kind, GqlErrorKind::Syntax);
    // Also through iterate() and next(): the marker fails the plan, not a row.
    assert!(empty.traversal().gql("MATCH").iterate().is_err());
    assert!(empty.traversal().gql("MATCH").next().is_err());
}

#[test]
fn the_error_names_the_marker_step() {
    let empty = TraversalGraph::new();
    let error = empty
        .traversal()
        .gql("MATCH (p) RETRUN p")
        .to_list()
        .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("gql:error"), "{text}");
}

#[test]
fn edge_patterns_are_not_lowered_yet() {
    let graph = TraversalGraph::tinkerpop_modern();
    let (kind, gqlstatus, _) = gql_error(
        graph
            .traversal()
            .gql("MATCH (a)-[:knows]->(b) RETURN b")
            .to_list(),
    );
    assert_eq!((kind, gqlstatus.as_str()), (GqlErrorKind::NotYet, "0A000"));
}

#[test]
fn gql_is_a_start_step() {
    let graph = TraversalGraph::tinkerpop_modern();
    let (kind, _, message) = gql_error(
        graph
            .traversal()
            .v(None)
            .gql("MATCH (p) RETURN p")
            .to_list(),
    );
    assert_eq!(kind, GqlErrorKind::NotYet);
    assert!(message.contains("starts a traversal"), "{message}");
}

// ----- the lowered plan -----

#[test]
fn the_label_folds_into_the_source_and_no_path_is_recorded() {
    let graph = TraversalGraph::tinkerpop_modern();
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name",
    );
    assert!(text.contains(r#"v(labels: ["person"])"#), "{text}");
    assert!(text.contains("P.gt(30)"), "{text}");
    assert!(!text.contains("has_label"), "{text}");
    assert!(
        !text.contains("[path:"),
        "no binding needs a path label: {text}"
    );
    assert!(!has_step(&text, "as("), "{text}");
    assert!(text.contains(r#"gql:project("name")"#), "{text}");
    // `=` is always P.eq, never the bare value (which misses 32.0 for 32).
    let text = profile(&graph, "MATCH (p:person {age: 32}) RETURN p");
    assert!(text.contains("P.eq(32)"), "{text}");
    // `<>`, IS NULL and expressions stay in gql:filter.
    let text = profile(&graph, "MATCH (p:person) WHERE p.age <> 29 RETURN p");
    assert!(text.contains(r#"gql:filter("p.age <> 29")"#), "{text}");
    assert!(!text.contains("P.neq"), "{text}");
    let text = profile(&graph, "MATCH (p:person) WHERE p.age IS NOT NULL RETURN p");
    assert!(text.contains("P.neq(())"), "{text}");
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE p.age IN [27, NULL, 32] RETURN p",
    );
    assert!(text.contains("P.within([27, 32])"), "{text}");
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE p.name STARTS WITH 'ma' RETURN p",
    );
    assert!(text.contains(r#"TextP.starting_with("ma")"#), "{text}");
    // A `$param` is pushed when its value fits.
    let text = graph
        .traversal()
        .gql_with(
            "MATCH (p:person) WHERE p.age IN $ages RETURN p",
            [("ages", ElementProperty::Array(vec![27.into()]))],
        )
        .profile()
        .unwrap()
        .to_string();
    assert!(text.contains("P.within([27])"), "{text}");
    // Label factors beyond plain names are checked by gql:filter.
    let text = profile(&graph, "MATCH (n:person&!software) RETURN n");
    assert!(text.contains(r#"v(labels: ["person"])"#), "{text}");
    assert!(text.contains(r#"gql:filter("!software")"#), "{text}");
    let text = profile(&graph, "MATCH (n:person|software) RETURN n");
    assert!(
        text.contains(r#"v(labels: ["person", "software"])"#),
        "{text}"
    );
}

#[test]
fn a_property_index_serves_an_inline_property() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .create_index(
            "person_name",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
        )
        .unwrap();
    let query = "MATCH (p:person {name: $n}) RETURN p.age AS age";
    let metrics = graph
        .traversal()
        .gql_with(query, [("n", "marko")])
        .profile()
        .unwrap()
        .to_string();
    assert!(metrics.contains("[index: vertex person.name]"), "{metrics}");
    assert!(metrics.contains("property_index"), "{metrics}");
    let result = rows_with(&graph, query, vec![("n", "marko".into())]).unwrap();
    assert_eq!(texts(&result, "age"), ["29"]);
}

#[test]
fn path_labels_only_where_a_later_step_reads_them() {
    let graph = TraversalGraph::tinkerpop_modern();
    // `a` is read after the second scan moved on; `s` is the current element.
    let text = profile(
        &graph,
        "MATCH (a:person {name: 'marko'}), (s:software) RETURN a.name AS a, s.name AS s",
    );
    assert!(has_step(&text, r#"as("a")"#), "{text}");
    assert!(!has_step(&text, r#"as("s")"#), "{text}");
    assert!(text.contains("labels(a)"), "{text}");
    // ORDER BY on a variable that is not returned reads it through its label.
    let text = profile(&graph, "MATCH (p:person) RETURN p.name AS n ORDER BY p.age");
    assert!(has_step(&text, r#"as("p")"#), "{text}");
    assert!(text.contains("gql:order"), "{text}");
    // ORDER BY on a returned column reads the row: no label.
    let text = profile(
        &graph,
        "MATCH (p:person) RETURN p.name AS n ORDER BY n DESC",
    );
    assert!(!has_step(&text, "as("), "{text}");
}

#[test]
fn distinct_is_a_dedup_after_the_projection() {
    let graph = TraversalGraph::tinkerpop_modern();
    let text = profile(&graph, "MATCH (p:person) RETURN DISTINCT 1 AS one");
    assert!(text.contains("dedup()"), "{text}");
    let result = rows(&graph, "MATCH (p:person) RETURN DISTINCT 1 AS one");
    assert_eq!(result.len(), 1);
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
        assert!(
            !left.is_empty() || twin.contains("EMPTY"),
            "{query}: no rows to compare"
        );
    }

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

    const NAME: &str = r#"project("n").by(__.coalesce(__.values("name"), __.constant(())))"#;
    const NAME_AGE: &str = r#"project("n", "a").by(__.coalesce(__.values("name"), __.constant(()))).by(__.coalesce(__.values("age"), __.constant(())))"#;

    #[test]
    fn node_scans_and_where_match_their_twins() {
        let graph = fixture();
        let persons = r#"g.V().hasLabel("person")"#;
        let cases: Vec<(String, String)> = vec![
            (
                "MATCH (p:person) RETURN p.name AS n, p.age AS a".into(),
                format!("{persons}.{NAME_AGE}"),
            ),
            (
                "MATCH (p:person) WHERE p.age > 30 RETURN p.name AS n".into(),
                format!(r#"{persons}.has("age", P.gt(30)).{NAME}"#),
            ),
            (
                "MATCH (p:person {age: 32}) RETURN p.name AS n".into(),
                format!(r#"{persons}.has("age", P.eq(32)).{NAME}"#),
            ),
            (
                "MATCH (p:person) WHERE p.age <> 29 RETURN p.name AS n".into(),
                format!(r#"{persons}.has("age", P.lt(29).or(P.gt(29))).{NAME}"#),
            ),
            (
                "MATCH (p:person) WHERE NOT (p.age > 30) RETURN p.name AS n".into(),
                format!(r#"{persons}.has("age", P.lte(30)).{NAME}"#),
            ),
            (
                "MATCH (p:person) WHERE p.age < 28 OR p.name = 'josh' RETURN p.name AS n".into(),
                format!(
                    r#"{persons}.or(__.has("age", P.lt(28)), __.has("name", "josh")).{NAME}"#
                ),
            ),
            (
                "MATCH (p:person) WHERE p.age IS NULL RETURN p.name AS n".into(),
                format!(
                    r#"{persons}.or(__.hasNot("age"), __.has("age", P.eq(()))).{NAME}"#
                ),
            ),
            (
                "MATCH (p:person) WHERE p.age IS NOT NULL RETURN p.name AS n".into(),
                format!(r#"{persons}.has("age", P.neq(())).{NAME}"#),
            ),
            (
                "MATCH (p:person) WHERE p.age = 'x' RETURN p.name AS n".into(),
                format!(r#"{persons}.has("age", "x").{NAME}"#),
            ),
            (
                "MATCH (p:person) WHERE p.age IN [29, NULL, 32] RETURN p.name AS n".into(),
                format!(r#"{persons}.has("age", P.within([29, 32])).{NAME}"#),
            ),
            (
                "MATCH (p) WHERE p.name STARTS WITH 'r' OR p.name ENDS WITH 'p' RETURN p.name AS n"
                    .into(),
                r#"g.V().or(__.has("name", TextP.starting_with("r")), __.has("name", TextP.ending_with("p"))).project("n").by(__.coalesce(__.values("name"), __.constant(())))"#.into(),
            ),
            (
                "MATCH (n) WHERE n.lang = 'java' AND n.name CONTAINS 'i' RETURN n.name AS n".into(),
                r#"g.V().has("lang", "java").has("name", TextP.containing("i")).project("n").by(__.coalesce(__.values("name"), __.constant(())))"#.into(),
            ),
            (
                "MATCH (n:person|software) RETURN n.name AS n".into(),
                format!(r#"g.V().hasLabel("person", "software").{NAME}"#),
            ),
            (
                "MATCH (n:!person) RETURN n.name AS n".into(),
                format!(r#"g.V().not(__.hasLabel("person")).{NAME}"#),
            ),
            (
                "MATCH (n:person&employee) RETURN n.name AS n".into(),
                format!(r#"g.V().hasLabel("person").hasLabel("employee").{NAME}"#),
            ),
            (
                "MATCH (p:person) RETURN DISTINCT p.age AS a".into(),
                r#"g.V().hasLabel("person").project("a").by(__.coalesce(__.values("age"), __.constant(()))).dedup()"#.into(),
            ),
            (
                "MATCH (p:person {name: 'marko'}) RETURN *".into(),
                r#"g.V().has("person", "name", "marko").project("p").by(__.identity())"#.into(),
            ),
            (
                "MATCH (a:person), (s:software) WHERE a.age > 30 RETURN a.name AS a, s.name AS s"
                    .into(),
                r#"g.V().hasLabel("person").has("age", P.gt(30)).as("a").V().hasLabel("software").as("s").select("a", "s").by("name")"#.into(),
            ),
        ];
        for (query, twin) in &cases {
            same(&graph, query, twin, false);
        }
    }

    #[test]
    fn ordered_results_match_their_twins() {
        let graph = fixture();
        // Without the string age, so the twin's NULL-aware key orders like GQL.
        let numeric = r#"g.V().hasLabel("person").not(__.has("name", "strage"))"#;
        let null_last = r#"order().by(__.coalesce(__.values("age").is(P.neq(())).constant(0), __.constant(1))).by(__.coalesce(__.values("age"), __.constant(()))).by("name")"#;
        let null_first = r#"order().by(__.coalesce(__.values("age").is(P.neq(())).constant(0), __.constant(1)), Order.desc).by(__.coalesce(__.values("age"), __.constant(())), Order.desc).by("name")"#;
        let cases: Vec<(String, String)> = vec![
            (
                "MATCH (p:person) WHERE p.name <> 'strage' RETURN p.name AS n, p.age AS a ORDER BY a, n"
                    .into(),
                format!("{numeric}.{null_last}.{NAME_AGE}"),
            ),
            (
                "MATCH (p:person) WHERE p.name <> 'strage' RETURN p.name AS n ORDER BY p.age DESC, p.name"
                    .into(),
                format!("{numeric}.{null_first}.{NAME}"),
            ),
            (
                "MATCH (p:person) WHERE p.name <> 'strage' RETURN p.name AS n ORDER BY p.age, n SKIP 2 LIMIT 3"
                    .into(),
                format!("{numeric}.{null_last}.range(2, 5).{NAME}"),
            ),
            (
                "MATCH (s:software) RETURN DISTINCT s.lang AS l ORDER BY l".into(),
                r#"g.V().hasLabel("software").values("lang").dedup().order().project("l").by(__.identity())"#.into(),
            ),
        ];
        for (query, twin) in &cases {
            same(&graph, query, twin, true);
        }
    }

    #[test]
    fn large_graph_queries_match_their_twins() {
        let graph = Arc::new(GraphSource::large_110k());
        same(
            &graph,
            "MATCH (v) WHERE v.level = '1' RETURN element_id(v) AS id",
            r#"g.V().has("level", "1").project("id").by(__.id())"#,
            false,
        );
        same(
            &graph,
            "MATCH (v {level: '2'}) RETURN element_id(v) AS id ORDER BY id DESC LIMIT 5",
            r#"g.V().has("level", "2").id().order().by(Order.desc).limit(5).project("id").by(__.identity())"#,
            true,
        );
        same(
            &graph,
            "MATCH (v) WHERE v.level IN ['0', '1'] RETURN DISTINCT v.level AS l",
            r#"g.V().has("level", P.within(["0", "1"])).values("level").dedup().project("l").by(__.identity())"#,
            false,
        );
    }
}
