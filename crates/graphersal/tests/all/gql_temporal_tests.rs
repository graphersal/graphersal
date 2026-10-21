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

/// 3g.T7: a vertex `t` holding one value of each type: `d` = 1984-10-11, `l` =
/// 1984-10-11T12:31:14.645876, `z` = the same wall clock at -01:00, `t` = 12:31:14.645876, `y` =
/// 1984-01-01 (ISO week-year 1983), `u` = a duration.
fn truncation_graph() -> TraversalGraph {
    use graphersal::temporal::{DateTime, LocalDateTime, Time};
    let mut graph = TraversalGraph::new();
    let day = Date::from_ymd(1984, 10, 11).expect("a date");
    let time = Time::from_hms_micro(12, 31, 14, 645_876).expect("a time");
    let local = LocalDateTime::new(day, time);
    let zoned = DateTime::from_local(local, -3600).expect("a datetime");
    let value = |t: Temporal| ElementProperty::Temporal(t);
    graph
        .traversal_mut()
        .add_v("t")
        .property("d", value(Temporal::Date(day)))
        .property("l", value(Temporal::LocalDateTime(local)))
        .property("z", value(Temporal::DateTime(zoned)))
        .property("t", value(Temporal::Time(time)))
        .property(
            "y",
            value(Temporal::Date(Date::from_ymd(1984, 1, 1).expect("a date"))),
        )
        .property("u", value(Temporal::Duration(Duration::new(1, 0, 0))))
        .iterate()
        .expect("the vertex");
    graph
}

/// The engine text of the single result of `expr` on the `t` vertex.
fn truncated(graph: &TraversalGraph, expr: &str) -> String {
    let query = format!("MATCH (n:t) RETURN {expr} AS x");
    match single(graph, &query) {
        ElementProperty::Temporal(value) => value.to_string(),
        other => panic!("{query}: {other:?}"),
    }
}

#[test]
fn truncate_follows_the_tck_units_and_projections() {
    let graph = truncation_graph();
    for (expr, expected) in [
        // Millennium and century as Neo4j (U4), not PostgreSQL's 2001 / 1901.
        ("date.truncate('millennium', n.d)", "1000-01-01"),
        ("date.truncate('century', n.d)", "1900-01-01"),
        ("date.truncate('decade', n.d)", "1980-01-01"),
        ("date.truncate('year', n.l)", "1984-01-01"),
        ("date.truncate('weekYear', n.y)", "1983-01-03"),
        ("date.truncate('weekYear', n.y, {day: 5})", "1983-01-05"),
        ("date.truncate('quarter', n.z)", "1984-10-01"),
        ("date.truncate('month', n.d, {day: 2})", "1984-10-02"),
        ("date.truncate('week', n.d)", "1984-10-08"),
        ("date.truncate('week', n.d, {dayOfWeek: 2})", "1984-10-09"),
        ("date.truncate('day', n.z)", "1984-10-11"),
        (
            "localdatetime.truncate('century', n.d)",
            "1900-01-01T00:00:00",
        ),
        ("localdatetime.truncate('hour', n.z)", "1984-10-11T12:00:00"),
        (
            "localdatetime.truncate('millisecond', n.l)",
            "1984-10-11T12:31:14.645",
        ),
        ("localtime.truncate('day', n.l)", "00:00:00"),
        ("localtime.truncate('minute', n.t)", "12:31:00"),
        ("localtime.truncate('second', n.z)", "12:31:14"),
        (
            "localtime.truncate('millisecond', n.t, {microsecond: 7})",
            "12:31:14.645007",
        ),
        ("localtime.truncate('nanosecond', n.t)", "12:31:14.645876"),
        // A datetime keeps its offset; a value without one is UTC; a zone in the map keeps the
        // wall clock and gives the zone's offset at that time (U3).
        (
            "datetime.truncate('hour', n.z)",
            "1984-10-11T12:00:00-01:00",
        ),
        ("datetime.truncate('year', n.d)", "1984-01-01T00:00:00Z"),
        ("datetime.truncate('day', n.l)", "1984-10-11T00:00:00Z"),
        (
            "datetime.truncate('hour', n.z, {timezone: 'Europe/Stockholm'})",
            "1984-10-11T12:00:00+01:00",
        ),
        (
            "datetime.truncate('month', n.d, {timezone: '+05:00'})",
            "1984-10-01T00:00:00+05:00",
        ),
    ] {
        assert_eq!(truncated(&graph, expr), expected, "{expr}");
    }
}

#[test]
fn truncate_errors_and_null() {
    let graph = truncation_graph();
    for (expr, status, needle) in [
        (
            "date.truncate('hour', n.d)",
            "22023",
            "the unit hour is not defined for a date",
        ),
        (
            "localtime.truncate('month', n.t)",
            "22023",
            "not defined for a local time",
        ),
        (
            "date.truncate('fortnight', n.d)",
            "22023",
            "unknown unit 'fortnight'",
        ),
        (
            "date.truncate('day', n.t)",
            "22G03",
            "takes a value with a date, not a local time",
        ),
        (
            "localtime.truncate('hour', n.d)",
            "22G03",
            "with a time of day, not a date",
        ),
        ("datetime.truncate('day', n.u)", "22G03", "not a duration"),
        ("date.truncate(1, n.d)", "22G03", "takes a unit name"),
        ("date.truncate('day', 1)", "22G03", "takes a temporal value"),
        (
            "date.truncate('day', n.d, {hour: 1})",
            "22007",
            "hour is not a field of a date",
        ),
        (
            "date.truncate('day', n.d, {day: 1, week: 2})",
            "22007",
            "the date fields conflict",
        ),
        (
            "date.truncate('month', n.d, {day: 32})",
            "22008",
            "the field day cannot be 32",
        ),
        (
            "datetime.truncate('day', n.z, {timezone: 'Mars/Olympus'})",
            "22007",
            "unknown time zone",
        ),
        (
            "localtime.truncate('second', n.t, {nanosecond: 2})",
            "0A000",
            "below the microsecond",
        ),
    ] {
        let query = format!("MATCH (n:t) RETURN {expr} AS x");
        let (actual, _, message, compile_time) = failure(&graph, &query);
        assert_eq!(actual, status, "{query}: {message}");
        assert!(message.contains(needle), "{query}: {message}");
        assert!(!compile_time, "{query}");
    }
    for expr in [
        "date.truncate('day', NULL)",
        "date.truncate(NULL, n.d)",
        "date.truncate('day', n.missing)",
        "date.truncate('day', n.d, NULL)",
        "date.truncate('day', n.d, {day: NULL})",
    ] {
        let query = format!("MATCH (n:t) RETURN {expr} AS x");
        assert_eq!(single(&graph, &query), ElementProperty::Null, "{query}");
    }
    // The Rhai / Gremlin date_trunc keeps PostgreSQL's millennium and century (1001, 1901).
    for (unit, expected) in [(DT::Millennium, "1001-01-01"), (DT::Century, "1901-01-01")] {
        let values = graph
            .traversal()
            .v(None)
            .has_label("t")
            .values("d")
            .date_trunc(unit)
            .to_list()
            .expect("the date");
        match values.as_slice() {
            [ElementProperty::Temporal(value)] => assert_eq!(value.to_string(), expected),
            other => panic!("{other:?}"),
        }
    }
}
