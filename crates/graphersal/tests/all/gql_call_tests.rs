//! GQL calls of saved queries (stage 3h, feature `gql`): `CALL folder.name(args) [YIELD ..]`.
//! Standalone and in-query calls, `YIELD` (renames, swaps, `*`, `WHERE`), VOID queries, implicit
//! arguments, defaults, the positional order (first use of each `$name`), every compile-time
//! error (not found, another folder, arity, types, missing values, aggregates and reads of the
//! row in arguments, `YIELD` mistakes, a Rhai body, recursion, a read-only query that writes),
//! writes through a call (per row, refused on a read source), calls in `CALL { }` bodies and
//! `UNION` operands, the isolation of the callee's variables, bulk, the profile, and saved GQL
//! queries that call each other (effects, dialect, the catalog check at save time).
#![cfg(feature = "gql")]

use std::sync::Arc;

use graphersal::catalog::{
    Definition, GQL_QUERY_DIALECT, GQL_WRITING_QUERY_DIALECT, QueryDefinition, QueryParam,
};
use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;
use graphersal::schema::{JsonType, PropertySchema};
use graphersal::storage::GraphStorage;

// ----- helpers -----

/// A required parameter of type `ty`, nullable when `nullable`.
fn param(ty: JsonType, nullable: bool) -> QueryParam {
    QueryParam::new(if nullable {
        PropertySchema::of_types(&[ty, JsonType::Null])
    } else {
        PropertySchema::of(ty)
    })
}

/// Stores the saved GQL query `name` in `folder` (`None`: the root) with `body` and `params`
/// (declaration order), in the dialect of a query that only reads.
fn save(
    graph: &mut TraversalGraph,
    folder: Option<&str>,
    name: &str,
    body: &str,
    params: Vec<(&str, QueryParam)>,
) {
    save_in(graph, folder, name, body, params, GQL_QUERY_DIALECT);
}

fn save_in(
    graph: &mut TraversalGraph,
    folder: Option<&str>,
    name: &str,
    body: &str,
    params: Vec<(&str, QueryParam)>,
    dialect: &str,
) {
    let mut query = QueryDefinition::new(body).with_dialect(dialect);
    if let Some(folder) = folder {
        query = query.with_folder(folder);
    }
    for (p, param) in params {
        query = query.with_param(p, param);
    }
    graph
        .set_definition(Definition::query(name, query))
        .unwrap_or_else(|e| panic!("{name}: {e}"));
}

/// The modern graph with `people.adults($min)` (persons at least `min` old, name and age by
/// name), `t.labels()` (`A`, `B`, `C`), `t.nothing()` (VOID) and `t.pair($a)` (`a`, `b`).
fn fixture() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    save(
        &mut graph,
        Some("people"),
        "adults",
        "MATCH (p:person) WHERE p.age >= $min RETURN p.name AS name, p.age AS age ORDER BY name",
        vec![("min", param(JsonType::Integer, false))],
    );
    save(
        &mut graph,
        Some("t"),
        "labels",
        "FOR label IN ['A', 'B', 'C'] RETURN label",
        vec![],
    );
    save(
        &mut graph,
        Some("t"),
        "nothing",
        "FOR x IN [] FINISH",
        vec![],
    );
    save(
        &mut graph,
        Some("t"),
        "pair",
        "RETURN $a AS a, $a + 1 AS b",
        vec![("a", param(JsonType::Integer, true))],
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
) -> Vec<ElementProperty> {
    graph
        .traversal()
        .gql_with(query, params)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

/// Every row as `key=value` pairs in column order.
fn table(rows: &[ElementProperty]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            ElementProperty::Object(map) => map
                .iter()
                .map(|(k, v)| format!("{k}={}", text(v)))
                .collect::<Vec<_>>()
                .join(" "),
            other => panic!("not a row: {other:?}"),
        })
        .collect()
}

fn text(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => s.to_string(),
        ElementProperty::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => n.as_f64().unwrap_or_default().to_string(),
        },
        other => format!("{other:?}"),
    }
}

/// The GQL error of `query`: kind, GQLSTATUS, message.
fn error_of(
    graph: &TraversalGraph,
    query: &str,
    params: Vec<(&str, ElementProperty)>,
) -> (GqlErrorKind, String, String) {
    let error = match graph.traversal().gql_with(query, params).to_list() {
        Ok(rows) => panic!("{query}: expected an error, got {rows:?}"),
        Err(error) => error,
    };
    match error.root_cause() {
        TraverserError::Gql {
            kind,
            gqlstatus,
            message,
            ..
        } => (*kind, gqlstatus.clone(), message.clone()),
        other => panic!("{query}: not a GQL error: {other}"),
    }
}

fn assert_error(graph: &TraversalGraph, query: &str, status: &str, part: &str) {
    let (_, gqlstatus, message) = error_of(graph, query, vec![]);
    assert_eq!(gqlstatus, status, "{query}: {message}");
    assert!(
        message.contains(part),
        "{query}: `{part}` not in: {message}"
    );
}

// ----- forms -----

#[test]
fn a_standalone_call_returns_every_column_in_the_query_order() {
    let graph = fixture();
    let expected = ["name=josh age=32", "name=peter age=35"];
    assert_eq!(table(&rows(&graph, "CALL people.adults(30)")), expected);
    assert_eq!(
        table(&rows(&graph, "CALL people.adults(30) YIELD *")),
        expected
    );
    assert_eq!(
        table(&rows(&graph, "CALL people.adults(30) YIELD age")),
        ["age=32", "age=35"]
    );
    // Its result equals the saved query run as gql().
    assert_eq!(
        rows(&graph, "CALL people.adults(30)"),
        rows_with(
            &graph,
            "MATCH (p:person) WHERE p.age >= $min RETURN p.name AS name, p.age AS age ORDER BY name",
            vec![("min", 30.into())]
        )
    );
}

#[test]
fn yield_selects_renames_and_swaps_columns() {
    let graph = fixture();
    assert_eq!(
        table(&rows(&graph, "CALL t.pair(1) YIELD b, a RETURN a, b")),
        ["a=1 b=2"]
    );
    assert_eq!(
        table(&rows(
            &graph,
            "CALL t.pair(1) YIELD a AS b, b AS a RETURN a, b"
        )),
        ["a=2 b=1"]
    );
    assert_eq!(
        table(&rows(&graph, "CALL t.pair(1) YIELD a AS x RETURN x")),
        ["x=1"]
    );
    // RETURN * after YIELD: the yielded names.
    assert_eq!(
        table(&rows(&graph, "CALL t.pair(5) YIELD b RETURN *")),
        ["b=6"]
    );
}

#[test]
fn an_in_query_call_runs_per_row() {
    let graph = fixture();
    let got = rows(
        &graph,
        "MATCH (s:software) CALL people.adults(33) YIELD name RETURN s.name AS s, name ORDER BY s",
    );
    assert_eq!(table(&got), ["s=lop name=peter", "s=ripple name=peter"]);
    // The callee's variables never meet the caller's: both bind `p`.
    let got = rows(
        &graph,
        "MATCH (p:software) CALL people.adults(35) YIELD name RETURN p.name AS p, name ORDER BY p",
    );
    assert_eq!(table(&got), ["p=lop name=peter", "p=ripple name=peter"]);
    // A call first, then MATCH.
    let got = rows(
        &graph,
        "CALL people.adults(35) YIELD name MATCH (p:person {name: name})-[:created]->(s) \
         RETURN s.name AS s",
    );
    assert_eq!(table(&got), ["s=lop"]);
    // Bulk: every incoming row gives its own records.
    let got = rows(
        &graph,
        "FOR x IN [1, 1, 1] CALL t.labels() YIELD label RETURN count(*) AS n",
    );
    assert_eq!(table(&got), ["n=9"]);
}

#[test]
fn yield_where_filters_the_records() {
    let graph = fixture();
    let got = rows(
        &graph,
        "CALL people.adults(0) YIELD name, age WHERE age > 30 RETURN name",
    );
    assert_eq!(table(&got), ["name=josh", "name=peter"]);
}

#[test]
fn a_call_without_return_keeps_each_row_once() {
    let graph = fixture();
    let got = rows(
        &graph,
        "MATCH (s:software) CALL t.nothing() RETURN s.name AS s ORDER BY s",
    );
    assert_eq!(table(&got), ["s=lop", "s=ripple"]);
    // Standalone: no row, no column.
    assert!(rows(&graph, "CALL t.nothing()").is_empty());
    assert!(rows(&graph, "CALL t.nothing").is_empty());
    // YIELD on it is an error.
    assert_error(
        &graph,
        "MATCH (s) CALL t.nothing() YIELD x RETURN s",
        "42002",
        "returns no columns",
    );
}

#[test]
fn optional_call_keeps_a_row_without_records() {
    let graph = fixture();
    let got = rows(
        &graph,
        "FOR m IN [30, 99] OPTIONAL CALL people.adults(99) YIELD name RETURN m, name ORDER BY m",
    );
    assert_eq!(table(&got), ["m=30 name=null", "m=99 name=null"]);
    let got = rows(
        &graph,
        "FOR m IN [1] OPTIONAL CALL people.adults(35) YIELD name RETURN m, name",
    );
    assert_eq!(table(&got), ["m=1 name=peter"]);
}

#[test]
fn calls_stand_in_call_bodies_union_operands_and_after_next() {
    let graph = fixture();
    let got = rows(
        &graph,
        "MATCH (s:software) CALL (s) { CALL people.adults(35) YIELD name RETURN name } \
         RETURN s.name AS s, name ORDER BY s",
    );
    assert_eq!(table(&got), ["s=lop name=peter", "s=ripple name=peter"]);
    let got = rows(
        &graph,
        "CALL t.labels() YIELD label RETURN label AS x \
         UNION ALL CALL people.adults(35) YIELD name RETURN name AS x",
    );
    assert_eq!(table(&got), ["x=A", "x=B", "x=C", "x=peter"]);
    let got = rows(
        &graph,
        "RETURN 35 AS m NEXT CALL people.adults(35) YIELD name RETURN m, name",
    );
    assert_eq!(table(&got), ["m=35 name=peter"]);
    // The same saved query twice, a WITH between (TCK Call6 [1]).
    let got = rows(
        &graph,
        "CALL t.labels() YIELD label WITH count(*) AS c CALL t.labels() YIELD label RETURN *",
    );
    assert_eq!(table(&got), ["c=3 label=A", "c=3 label=B", "c=3 label=C"]);
}

// ----- arguments -----

#[test]
fn arguments_are_positional_in_the_order_of_first_use() {
    let mut graph = fixture();
    // Declared (b, a), read first `$a` then `$b`: `CALL t.minus(10, 3)` is a = 10, b = 3.
    save(
        &mut graph,
        Some("t"),
        "minus",
        "RETURN $a - $b AS d",
        vec![
            ("b", param(JsonType::Integer, false)),
            ("a", param(JsonType::Integer, false)),
        ],
    );
    assert_eq!(table(&rows(&graph, "CALL t.minus(10, 3)")), ["d=7"]);
    // Constant expressions and parameters.
    assert_eq!(
        table(&rows_with(
            &graph,
            "CALL t.minus($x * 2, size([1, 2]))",
            vec![("x", 5.into())]
        )),
        ["d=8"]
    );
}

#[test]
fn a_trailing_argument_left_out_takes_the_default() {
    let mut graph = fixture();
    save(
        &mut graph,
        None,
        "scaled",
        "RETURN $x * $k AS v",
        vec![
            ("x", param(JsonType::Integer, false)),
            (
                "k",
                QueryParam::new(PropertySchema::of(JsonType::Integer)).with_default(10),
            ),
        ],
    );
    assert_eq!(table(&rows(&graph, "CALL scaled(2)")), ["v=20"]);
    assert_eq!(table(&rows(&graph, "CALL scaled(2, 3)")), ["v=6"]);
    assert_error(&graph, "CALL scaled()", "42002", "missing argument for `x`");
}

#[test]
fn implicit_arguments_come_from_the_parameters_of_a_standalone_call() {
    let graph = fixture();
    assert_eq!(
        table(&rows_with(&graph, "CALL t.pair", vec![("a", 4.into())])),
        ["a=4 b=5"]
    );
    let (_, status, message) = error_of(&graph, "CALL t.pair", vec![]);
    assert_eq!(status, "42002");
    assert!(
        message.contains("the parameter `$a` has no value"),
        "{message}"
    );
    // In a query only without parameters.
    assert_eq!(
        table(&rows(
            &graph,
            "MATCH (s:software {name: 'lop'}) CALL t.labels YIELD label RETURN label"
        )),
        ["label=A", "label=B", "label=C"]
    );
    assert_error(
        &graph,
        "CALL t.pair YIELD a RETURN a",
        "42002",
        "passes its arguments in parentheses",
    );
}

#[test]
fn an_implicit_argument_the_caller_does_not_give_takes_its_default() {
    let mut graph = fixture();
    save(
        &mut graph,
        Some("t"),
        "older",
        "MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name",
        vec![("min", param(JsonType::Integer, false).with_default(30))],
    );
    assert_eq!(
        table(&rows(&graph, "CALL t.older")),
        ["name=josh", "name=peter"]
    );
    assert_eq!(
        table(&rows_with(&graph, "CALL t.older", vec![("min", 33.into())])),
        ["name=peter"]
    );
}

#[test]
fn an_argument_is_checked_against_the_parameter_schema() {
    let mut graph = fixture();
    save(
        &mut graph,
        None,
        "half",
        "RETURN $x / 2 AS h",
        vec![("x", param(JsonType::Number, false))],
    );
    // An integer for a number is coerced (42 -> 42.0).
    assert_eq!(table(&rows(&graph, "CALL half(5)")), ["h=2.5"]);
    assert_error(&graph, "CALL half('5')", "22G03", "must be number");
    // NULL only where the schema admits it.
    assert_error(&graph, "CALL half(null)", "22G03", "must be number");
    assert_eq!(table(&rows(&graph, "CALL t.pair(null)")), ["a=null b=null"]);
}

#[test]
fn arity_errors_name_the_signature() {
    let graph = fixture();
    assert_error(
        &graph,
        "CALL t.pair(1, 2, 3)",
        "42002",
        "too many arguments: t.pair(a :: integer or null) takes 1 argument, 3 given",
    );
    assert_error(
        &graph,
        "CALL people.adults() YIELD name RETURN name",
        "42002",
        "missing argument for `min`",
    );
}

#[test]
fn an_argument_is_a_constant() {
    let graph = fixture();
    assert_error(
        &graph,
        "MATCH (n) CALL t.pair(count(n)) YIELD a RETURN a",
        "42002",
        "cannot aggregate",
    );
    let (kind, status, message) = error_of(
        &graph,
        "MATCH (n:person) CALL t.pair(n.age) YIELD a RETURN a",
        vec![],
    );
    assert_eq!(
        (kind, status.as_str()),
        (GqlErrorKind::NotYet, "0A000"),
        "{message}"
    );
    assert!(message.contains("reads the row"), "{message}");
}

// ----- YIELD errors -----

#[test]
fn yield_mistakes_are_errors() {
    let graph = fixture();
    assert_error(
        &graph,
        "CALL t.pair(1) RETURN a",
        "42002",
        "names the columns it reads with YIELD",
    );
    assert_error(
        &graph,
        "CALL t.pair(1) YIELD * RETURN a",
        "42001",
        "YIELD * is allowed only in a standalone CALL",
    );
    assert_error(
        &graph,
        "CALL t.pair(1) YIELD c RETURN c",
        "42002",
        "`c` is not a column",
    );
    assert_error(
        &graph,
        "CALL t.pair(1) YIELD a, b AS a RETURN a",
        "42002",
        "`a` is yielded twice",
    );
    assert_error(
        &graph,
        "WITH 1 AS a CALL t.pair(1) YIELD a RETURN a",
        "42002",
        "yields `a`, which is already defined",
    );
}

// ----- resolution errors -----

#[test]
fn a_call_names_a_saved_query_and_its_folder() {
    let graph = fixture();
    assert_error(
        &graph,
        "CALL nowhere.adults(1)",
        "42002",
        "there is no saved query `adults` in the folder `nowhere`: the saved query `adults` \
         is in the folder `people`",
    );
    assert_error(
        &graph,
        "CALL adults(1)",
        "42002",
        "there is no saved query `adults` in the root folder",
    );
    assert_error(
        &graph,
        "CALL people.missing(1) YIELD name RETURN name",
        "42002",
        "there is no saved query `missing` in the folder `people`",
    );
    // Names are case-sensitive.
    assert_error(
        &graph,
        "CALL people.Adults(1)",
        "42002",
        "no saved query `Adults`",
    );
    // A back-quoted folder segment.
    let mut graph = fixture();
    save(
        &mut graph,
        Some("my reports"),
        "one",
        "RETURN 1 AS one",
        vec![],
    );
    assert_eq!(table(&rows(&graph, "CALL `my reports`.one()")), ["one=1"]);
}

#[test]
fn a_rhai_saved_query_is_refused_naming_g_query() {
    let mut graph = fixture();
    graph
        .set_definition(Definition::query(
            "rhai_one",
            QueryDefinition::new("fn rhai_one() { 1 }"),
        ))
        .unwrap();
    let (kind, status, message) = error_of(&graph, "CALL rhai_one()", vec![]);
    assert_eq!((kind, status.as_str()), (GqlErrorKind::NotYet, "0A000"));
    assert!(message.contains("is written in Rhai"), "{message}");
    let error = graph
        .traversal()
        .gql("CALL rhai_one()")
        .to_list()
        .unwrap_err();
    let help = error.help().unwrap_or_default();
    assert!(help.contains("g.query(\"rhai_one\""), "{help}");
}

#[test]
fn recursion_is_refused_before_the_run() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    save(
        &mut graph,
        None,
        "ping",
        "CALL pong() YIELD x RETURN x",
        vec![],
    );
    save(
        &mut graph,
        None,
        "pong",
        "CALL ping() YIELD x RETURN x",
        vec![],
    );
    assert_error(
        &graph,
        "CALL ping()",
        "42002",
        "recursive call of the saved query `ping`: ping calls pong calls ping",
    );
    // A chain deeper than 16 levels.
    let mut graph = TraversalGraph::tinkerpop_modern();
    save(&mut graph, None, "q0", "RETURN 1 AS x", vec![]);
    for i in 1..=20 {
        let body = format!("CALL q{}() YIELD x RETURN x", i - 1);
        save(&mut graph, None, &format!("q{i}"), &body, vec![]);
    }
    assert_eq!(table(&rows(&graph, "CALL q15()")), ["x=1"]);
    let (kind, status, message) = error_of(&graph, "CALL q20()", vec![]);
    assert_eq!(
        (kind, status.as_str()),
        (GqlErrorKind::Limit, "54001"),
        "{message}"
    );
    assert!(message.contains("more than 16 levels deep"), "{message}");
}

#[test]
fn a_read_only_saved_query_whose_query_writes_is_refused() {
    let mut graph = fixture();
    // Stored by hand in the read-only dialect (a stale or tampered catalog).
    save(&mut graph, None, "sneaky", "INSERT (:x)", vec![]);
    let (kind, _, message) = error_of(&graph, "CALL sneaky()", vec![]);
    assert_eq!(kind, GqlErrorKind::Refused);
    assert!(message.contains("a saved query is read-only"), "{message}");
}

// ----- writes -----

#[test]
fn a_call_of_a_writing_query_writes_per_row() {
    let mut graph = fixture();
    save_in(
        &mut graph,
        None,
        "plant",
        "INSERT (:fruit {name: $n})",
        vec![("n", param(JsonType::String, false))],
        GQL_WRITING_QUERY_DIALECT,
    );
    graph
        .traversal_mut()
        .gql("FOR i IN [1, 2, 3] CALL plant('apple') RETURN i")
        .iterate()
        .unwrap();
    graph
        .traversal_mut()
        .gql("CALL plant('pear')")
        .iterate()
        .unwrap();
    let names = graph
        .traversal()
        .v(None)
        .has_label("fruit")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names.len(), 4);
    // A read source refuses it like a Gremlin write, and nothing is written.
    assert!(
        graph
            .traversal()
            .gql("CALL plant('kiwi')")
            .to_list()
            .is_err()
    );
    let count = graph.traversal().v(None).has_label("fruit").count().next();
    assert_eq!(count.unwrap(), Some(ElementProperty::from(4_i64)));
}

#[test]
fn a_query_that_reads_after_a_writing_call_sees_its_writes() {
    let mut graph = fixture();
    save_in(
        &mut graph,
        None,
        "plant",
        "INSERT (:fruit {name: $n}) RETURN $n AS planted",
        vec![("n", param(JsonType::String, false))],
        GQL_WRITING_QUERY_DIALECT,
    );
    let got = graph
        .traversal_mut()
        .gql("CALL plant('fig') YIELD planted MATCH (f:fruit) RETURN planted, f.name AS f")
        .to_list()
        .unwrap();
    assert_eq!(table(&got), ["planted=fig f=fig"]);
}

#[test]
fn a_query_may_end_in_a_void_call() {
    let mut graph = fixture();
    save_in(
        &mut graph,
        None,
        "plant",
        "INSERT (:fruit {name: $n})",
        vec![("n", param(JsonType::String, false))],
        GQL_WRITING_QUERY_DIALECT,
    );
    // Like a last clause that writes: zero rows, the call runs once per row.
    let got = graph
        .traversal_mut()
        .gql("FOR i IN [1, 2, 3] CALL plant('apple')")
        .to_list()
        .unwrap();
    assert!(got.is_empty(), "{got:?}");
    let got = graph
        .traversal_mut()
        .gql("MATCH (s:software) CALL plant('pear');")
        .to_list()
        .unwrap();
    assert!(got.is_empty(), "{got:?}");
    let got = graph
        .traversal_mut()
        .gql("MATCH (s:software {name: 'lop'}) CALL (s) { CALL plant('fig') } RETURN s.name AS s")
        .to_list()
        .unwrap();
    assert_eq!(table(&got), ["s=lop"]);
    let count = graph.traversal().v(None).has_label("fruit").count().next();
    assert_eq!(count.unwrap(), Some(ElementProperty::from(6_i64)));
    // A VOID query that only reads: zero rows too.
    assert!(rows(&graph, "MATCH (s:software) CALL t.nothing()").is_empty());
    assert!(rows(&graph, "MATCH (s:software) OPTIONAL CALL t.nothing()").is_empty());
    // A called query that returns columns still needs YIELD and the query RETURN.
    assert_error(
        &graph,
        "MATCH (s:software) CALL people.adults(30)",
        "42002",
        "names the columns it reads with YIELD",
    );
    assert_error(
        &graph,
        "MATCH (s:software) CALL people.adults(30) YIELD name",
        "42001",
        "missing RETURN",
    );
}

#[test]
fn a_yielded_node_or_edge_starts_a_later_pattern() {
    let mut graph = fixture();
    save(
        &mut graph,
        None,
        "software",
        "MATCH (s:software) RETURN s, s.name AS name",
        vec![],
    );
    save(
        &mut graph,
        None,
        "knowing",
        "MATCH ()-[k:knows]->() RETURN k",
        vec![],
    );
    let got = rows(
        &graph,
        "CALL software() YIELD s MATCH (s)<-[:created]-(p) RETURN s.name AS s, p.name AS p \
         ORDER BY s, p",
    );
    assert_eq!(
        table(&got),
        [
            "s=lop p=josh",
            "s=lop p=marko",
            "s=lop p=peter",
            "s=ripple p=josh"
        ]
    );
    let got = rows(
        &graph,
        "CALL knowing() YIELD k MATCH ()-[k]->(b) RETURN b.name AS b ORDER BY b",
    );
    assert_eq!(table(&got), ["b=josh", "b=vadas"]);
    // A NULL gives no row; any other value that is not an element of the kind is an error
    // when a row reaches the pattern.
    assert!(rows(&graph, "CALL t.pair(NULL) YIELD a MATCH (a)-->(x) RETURN x").is_empty());
    let (_, status, message) = error_of(
        &graph,
        "CALL software() YIELD name MATCH (name)-->(x) RETURN x",
        vec![],
    );
    assert_eq!(status, "22G03", "{message}");
    assert!(
        message.contains(
            "`name` is written as a node of the pattern but holds a value that is not a node"
        ),
        "{message}"
    );
    let (_, status, message) = error_of(
        &graph,
        "CALL knowing() YIELD k MATCH (k)-->(x) RETURN x",
        vec![],
    );
    assert_eq!(status, "22G03", "{message}");
    assert!(
        message.contains("holds a value that is not a node"),
        "{message}"
    );
}

// ----- plan -----

#[test]
fn the_profile_shows_the_called_query_as_a_child() {
    let graph = fixture();
    let profile = graph
        .traversal()
        .gql("MATCH (s:software) CALL people.adults(32) YIELD name RETURN s.name AS s, name")
        .profile()
        .unwrap()
        .to_string();
    assert!(
        profile.contains(r#"gql:call_query("people/adults", __.v(labels: ["person"])"#),
        "{profile}"
    );
    assert!(profile.contains(r#"\> has("age", P.gte(32))"#), "{profile}");
}

// ----- saved queries calling saved queries (DSL) -----

#[cfg(feature = "script")]
mod saved {
    use super::*;
    use crate::script_host::eval_value;

    fn run(graph: &Arc<Graph>, script: &str) -> Result<String, String> {
        eval_value(graph.clone(), script)
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
    }

    fn ok(graph: &Arc<Graph>, script: &str) -> String {
        run(graph, script).unwrap_or_else(|e| panic!("{script}: {e}"))
    }

    /// `plant($n)` writes a fruit; `jobs/garden($n)` calls it.
    fn with_garden() -> Arc<Graph> {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        ok(
            &graph,
            r#"g.define_query(#{name: "plant", language: "gql", params: #{n: "string"},
                   body: "INSERT (:fruit {name: $n})"});
               g.define_query(#{name: "garden", folder: "jobs", language: "gql",
                   params: #{n: "string"}, body: "CALL plant($n) FINISH"})"#,
        );
        graph
    }

    fn fruits(graph: &Arc<Graph>) -> String {
        ok(
            graph,
            r#"g.v().has_label("fruit").values("name").order().to_list()"#,
        )
    }

    #[test]
    fn a_gql_query_calling_a_writing_one_writes_too() {
        let graph = with_garden();
        assert_eq!(
            ok(
                &graph,
                r#"g.queries().filter(|q| q.name == "garden").map(|q| [q.dialect, q.writes])"#
            ),
            format!(r#"[["{GQL_WRITING_QUERY_DIALECT}", true]]"#)
        );
        ok(&graph, r#"g.query("garden", #{n: "fig"}).iterate()"#);
        ok(&graph, r#"g.gql("CALL jobs.garden('kiwi')").iterate()"#);
        assert_eq!(fruits(&graph), r#"["fig", "kiwi"]"#);
    }

    #[test]
    fn a_rhai_body_calling_gql_with_a_call_infers_the_callee_effects() {
        let graph = with_garden();
        ok(
            &graph,
            r#"g.define_query(#{name: "seed", body: "fn seed() { g.gql(\"CALL plant('pea')\").iterate() }"})"#,
        );
        assert_eq!(
            ok(
                &graph,
                r#"g.queries().filter(|q| q.name == "seed").map(|q| q.writes)"#
            ),
            "[true]"
        );
        ok(&graph, r#"g.query("seed")"#);
        assert_eq!(fruits(&graph), r#"["pea"]"#);
    }

    #[test]
    fn a_body_that_is_only_an_implicit_call_reads_the_declared_parameters() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        ok(
            &graph,
            r#"g.define_query(#{name: "older", folder: "people", language: "gql",
                   params: #{min: #{schema: #{type: "integer"}, default: 30}},
                   body: "MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name"});
               g.define_query(#{name: "older2", language: "gql",
                   params: #{min: #{schema: #{type: "integer"}, default: 30}},
                   body: "CALL people.older"})"#,
        );
        assert_eq!(
            ok(
                &graph,
                r#"g.queries().filter(|q| q.name == "older2").map(|q| [q.status, q.params.map(|p| p.name)])"#
            ),
            r#"[["ok", ["min"]]]"#
        );
        assert_eq!(
            ok(&graph, r#"g.query("older2", #{min: 33}).to_list()"#),
            r#"[#{"name": "peter"}]"#
        );
        assert_eq!(
            ok(&graph, r#"g.query("older2").to_list()"#),
            r#"[#{"name": "josh"}, #{"name": "peter"}]"#
        );
        assert_eq!(
            ok(&graph, r#"g.gql("CALL older2(33)").to_list()"#),
            r#"[#{"name": "peter"}]"#
        );
        // The parameters of the call are checked against the declared ones as usual.
        let err = run(
            &graph,
            r#"g.define_query(#{name: "bad", language: "gql", params: #{other: "integer"},
                   body: "CALL people.older"})"#,
        )
        .unwrap_err();
        assert!(err.contains("reads the parameter $min"), "{err}");
    }

    #[test]
    fn saving_a_query_checks_its_calls_against_the_catalog() {
        let graph = with_garden();
        let err = run(
            &graph,
            r#"g.define_query(#{name: "bad", language: "gql", body: "CALL nowhere.plant('x') FINISH"})"#,
        )
        .unwrap_err();
        assert!(
            err.contains("there is no saved query `plant` in the folder `nowhere`"),
            "{err}"
        );
        // A constant argument is checked when the query is saved, ...
        let err = run(
            &graph,
            r#"g.define_query(#{name: "bad", language: "gql", body: "CALL plant(1) FINISH"})"#,
        )
        .unwrap_err();
        assert!(err.contains("must be string"), "{err}");
        // ... one reading a parameter when it is called.
        ok(
            &graph,
            r#"g.define_query(#{name: "later", language: "gql", params: #{v: "integer"},
                   body: "CALL plant($v) FINISH"})"#,
        );
        let err = run(&graph, r#"g.query("later", #{v: 1}).iterate()"#).unwrap_err();
        assert!(err.contains("must be string"), "{err}");
        assert_eq!(fruits(&graph), "[]");
    }
}
