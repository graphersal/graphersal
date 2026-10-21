//! GQL stage 3g, temporal functions (feature `gql`). 3g.T2: the names parse and bind as ordinary
//! functions (ISO `local_time` / `local_datetime` / `zoned_datetime` beside the Cypher names,
//! the qualified Cypher rows `date.truncate`, `duration.between`, ...), temporal values flow
//! through the evaluator (comparison, static kinds that never refuse a temporal operand where a
//! later task gives it a meaning), and every form a later task implements answers 0A000 "not
//! yet" at run time instead of a wrong value. Cypher's zoned `time()` stays "not yet" (U2),
//! `X.realtime()` is refused (U9).
#![cfg(feature = "gql")]

use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;
use graphersal::temporal::{Date, Duration, Temporal};

/// The modern graph plus one `x` vertex with a date `d` and a duration `u`.
fn graph() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let day = Date::from_ymd(2020, 1, 1).expect("a date");
    graph
        .traversal_mut()
        .add_v("x")
        .property("d", ElementProperty::Temporal(Temporal::Date(day)))
        .property(
            "u",
            ElementProperty::Temporal(Temporal::Duration(Duration::new(0, 1, 0))),
        )
        .iterate()
        .expect("the vertex");
    graph
}

fn run(graph: &TraversalGraph, query: &str) -> Result<Vec<ElementProperty>, Box<TraverserError>> {
    graph.traversal().gql(query).to_list()
}

fn rows(graph: &TraversalGraph, query: &str) -> Vec<ElementProperty> {
    run(graph, query).unwrap_or_else(|e| panic!("{query}: {e}"))
}

/// The failure of `query`: GQLSTATUS, kind, message, and whether it arose before the run.
fn failure(graph: &TraversalGraph, query: &str) -> (String, GqlErrorKind, String, bool) {
    let error = match run(graph, query) {
        Ok(rows) => panic!("{query} gave {rows:?}"),
        Err(error) => error,
    };
    let compile_time = error
        .to_string()
        .contains("refused the query before the traversal ran");
    match error.root_cause() {
        TraverserError::Gql {
            kind,
            gqlstatus,
            message,
            ..
        } => (gqlstatus.clone(), *kind, message.clone(), compile_time),
        other => panic!("{query}: not a GQL error: {other}"),
    }
}

/// The value of the single column `x` of the single row of `query`.
fn single(graph: &TraversalGraph, query: &str) -> ElementProperty {
    let rows = rows(graph, query);
    match rows.as_slice() {
        [ElementProperty::Map(map)] => map.values().next().cloned(),
        [ElementProperty::Object(map)] => map.values().next().cloned(),
        _ => panic!("{query}: {rows:?}"),
    }
    .unwrap_or(ElementProperty::Null)
}

#[test]
fn every_temporal_function_parses_binds_and_is_not_yet_at_run_time() {
    let graph = graph();
    for (call, name) in [
        ("date()", "date()"),
        ("date('2020-01-01')", "date()"),
        ("localtime('12:00')", "localtime()"),
        ("local_time()", "localtime()"),
        ("localdatetime({year: 1984})", "localdatetime()"),
        ("local_datetime()", "localdatetime()"),
        ("datetime()", "datetime()"),
        ("zoned_datetime('2020-01-01T00:00Z')", "datetime()"),
        ("duration('P1D')", "duration()"),
        ("date.statement()", "date.statement()"),
        ("datetime.transaction()", "datetime.transaction()"),
        ("localtime.statement()", "localtime.statement()"),
        ("localdatetime.transaction()", "localdatetime.transaction()"),
        ("date.truncate('month', n.d)", "date.truncate()"),
        (
            "localdatetime.truncate('day', n.d)",
            "localdatetime.truncate()",
        ),
        ("datetime.fromepoch(1, 0)", "datetime.fromepoch()"),
        ("datetime.fromepochmillis(1)", "datetime.fromepochmillis()"),
        ("duration.between(n.d, n.d)", "duration.between()"),
        ("duration.inMonths(n.d, n.d)", "duration.inMonths()"),
        ("duration.inDays(n.d, n.d)", "duration.inDays()"),
        ("duration.inSeconds(n.d, n.d)", "duration.inSeconds()"),
    ] {
        let query = format!("MATCH (n:x) RETURN {call} AS x");
        let (status, kind, message, compile_time) = failure(&graph, &query);
        assert_eq!(status, "0A000", "{query}");
        assert_eq!(kind, GqlErrorKind::NotYet, "{query}");
        assert!(!compile_time, "{query}: refused before the run");
        assert_eq!(
            message,
            format!("{name} is not yet supported in this version"),
            "{query}"
        );
        // The message names the Cypher spelling (the TCK's); no row reaching the call, no
        // error.
        let none = format!("MATCH (n:nothing) RETURN {call} AS x");
        assert!(rows(&graph, &none).is_empty(), "{none}");
    }
}

#[test]
fn zoned_time_and_realtime_are_refused_before_the_run() {
    let graph = graph();
    for (call, kind) in [
        ("time('12:00')", GqlErrorKind::NotYet),
        ("zoned_time()", GqlErrorKind::NotYet),
        ("time.truncate('hour', n.d)", GqlErrorKind::NotYet),
        ("time.statement()", GqlErrorKind::NotYet),
        ("datetime.realtime()", GqlErrorKind::Refused),
        ("date.realtime()", GqlErrorKind::Refused),
    ] {
        let query = format!("MATCH (n:x) RETURN {call} AS x");
        let (status, actual, _, compile_time) = failure(&graph, &query);
        assert_eq!(status, "0A000", "{query}");
        assert_eq!(actual, kind, "{query}");
        assert!(compile_time, "{query}");
    }
}

#[test]
fn temporal_values_flow_through_the_evaluator() {
    let graph = graph();
    // Stored values come back unchanged, compare within their type, and pass through
    // functions that take any value.
    assert!(matches!(
        single(&graph, "MATCH (n:x) RETURN n.d AS x"),
        ElementProperty::Temporal(Temporal::Date(_))
    ));
    for (expr, expected) in [
        ("n.d = n.d", true),
        ("n.d < n.d", false),
        ("n.d <= n.d", true),
        ("n.u = n.u", true),
        ("n.d IN [n.d]", true),
        ("coalesce(NULL, n.d) = n.d", true),
        ("n.d IS NULL", false),
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        assert_eq!(
            single(&graph, &query),
            ElementProperty::Boolean(expected),
            "{query}"
        );
    }
    let collected = single(&graph, "MATCH (n:x) RETURN collect_list(n.d) AS x");
    assert!(
        matches!(&collected, ElementProperty::Array(items) if items.len() == 1),
        "{collected:?}"
    );
    // Components and arithmetic come in later tasks: 0A000 at run time, never 22G03.
    for (expr, message) in [
        ("n.d.year", "the component year of a date"),
        ("n.u.days", "the component days of a duration"),
        ("n.d + n.u", "+ on temporal values"),
        ("n.u + n.d", "+ on temporal values"),
        ("n.d - n.u", "- on temporal values"),
        ("n.u * 2", "* on temporal values"),
        ("2 * n.u", "* on temporal values"),
        ("n.u / 2", "/ on temporal values"),
        ("-n.u", "the sign - of a temporal value"),
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        let (status, kind, actual, compile_time) = failure(&graph, &query);
        assert_eq!(status, "0A000", "{query}");
        assert_eq!(kind, GqlErrorKind::NotYet, "{query}");
        assert!(!compile_time, "{query}");
        assert_eq!(
            actual,
            format!("{message} is not yet supported in this version"),
            "{query}"
        );
    }
    // NULL still wins before the temporal operand is looked at.
    assert_eq!(
        single(&graph, "MATCH (n:x) RETURN n.d + NULL AS x"),
        ElementProperty::Null
    );
    assert_eq!(
        single(&graph, "MATCH (n:x) RETURN n.missing - n.u AS x"),
        ElementProperty::Null
    );
    // Operators no temporal value takes keep their 22G03, naming the kind.
    for (expr, needle) in [
        ("n.d % 2", "not a date and a number"),
        ("size(n.u)", "not a duration"),
        ("n.d || 'x'", "not a date and a string"),
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        let (status, _, message, _) = failure(&graph, &query);
        assert_eq!(status, "22G03", "{query}");
        assert!(message.contains(needle), "{query}: {message}");
    }
}

#[test]
fn static_kinds_accept_temporal_results_where_they_will_mean_something() {
    let graph = graph();
    // No static refusal: a component read, the arithmetic of T6, the sign; each fails only when
    // a row reaches it (and not at all without rows).
    for expr in [
        "date('2020-01-01').year",
        "duration.between(n.d, n.d).days",
        "date() - duration('P1D')",
        "duration('P1D') * 2",
        "duration('P1D') / 2",
        "-duration('P1D')",
        "datetime.truncate('day', n.d).hour",
    ] {
        let none = format!("MATCH (n:nothing) RETURN {expr} AS x");
        assert!(rows(&graph, &none).is_empty(), "{none}");
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        let (status, _, _, compile_time) = failure(&graph, &query);
        assert_eq!(status, "0A000", "{query}");
        assert!(!compile_time, "{query}");
    }
    // A temporal result is still no boolean, string, list or number.
    for query in [
        "RETURN date() AND true AS x",
        "RETURN NOT duration('P1D') AS x",
        "RETURN size(date()) AS x",
        "RETURN upper(datetime()) AS x",
        "RETURN duration('P1D') % 2 AS x",
        "RETURN keys(date()) AS x",
        "MATCH (n) WHERE date() RETURN n",
    ] {
        let (status, _, _, compile_time) = failure(&graph, query);
        assert_eq!(status, "22G03", "{query}");
        assert!(compile_time, "{query}");
    }
}

#[test]
fn names_like_temporal_functions_stay_variables_and_properties() {
    let graph = graph();
    assert_eq!(
        rows(
            &graph,
            "MATCH (date:person) RETURN date.name AS n ORDER BY n"
        ),
        rows(&graph, "MATCH (p:person) RETURN p.name AS n ORDER BY n")
    );
    assert_eq!(
        rows(
            &graph,
            "MATCH (duration:person) WITH duration.age AS between RETURN between ORDER BY between"
        ),
        rows(
            &graph,
            "MATCH (p:person) RETURN p.age AS between ORDER BY between"
        )
    );
    // A namespaced call that is no temporal row stays the namespaced refusal.
    let (status, kind, message, compile_time) =
        failure(&graph, "MATCH (date:person) RETURN date.name() AS n");
    assert_eq!(status, "0A000");
    assert_eq!(kind, GqlErrorKind::Refused);
    assert!(message.contains("namespaced functions"), "{message}");
    assert!(compile_time);
}
