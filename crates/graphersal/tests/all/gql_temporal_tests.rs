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

/// 3g.T8: one `t` vertex holding the TCK's Temporal10 operands `l0..l3` / `r0..r3` (a date, a
/// local time, a local datetime and a datetime each side) and a duration `u`.
fn between_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let values = [
        ("l0", graphersal::temporal::TemporalKind::Date, "1984-10-11"),
        ("l1", graphersal::temporal::TemporalKind::Time, "14:30"),
        (
            "l2",
            graphersal::temporal::TemporalKind::LocalDateTime,
            "2015-07-21T21:40:32.142",
        ),
        (
            "l3",
            graphersal::temporal::TemporalKind::DateTime,
            "2014-07-21T21:40:36.143+02:00",
        ),
        ("r0", graphersal::temporal::TemporalKind::Date, "2015-06-24"),
        (
            "r1",
            graphersal::temporal::TemporalKind::LocalDateTime,
            "2016-07-21T21:45:22.142",
        ),
        (
            "r2",
            graphersal::temporal::TemporalKind::DateTime,
            "2015-07-21T21:40:32.142+01:00",
        ),
        ("r3", graphersal::temporal::TemporalKind::Time, "16:30"),
    ];
    let mut step = graph.traversal_mut().add_v("t");
    for (key, kind, text) in values {
        let value = Temporal::parse_as(kind, text).expect("a temporal value");
        step = step.property(key, ElementProperty::Temporal(value));
    }
    step.property(
        "u",
        ElementProperty::Temporal(Temporal::Duration(Duration::new(0, 1, 0))),
    )
    .iterate()
    .expect("the vertex");
    graph
}

/// Microseconds of `h` hours, `m` minutes and `ms` milliseconds.
fn hms(h: i64, m: i64, ms: i64) -> i64 {
    ((h * 60 + m) * 60_000 + ms) * 1000
}

fn duration_of(graph: &TraversalGraph, expr: &str) -> ElementProperty {
    single(graph, &format!("MATCH (n:t) RETURN {expr} AS x"))
}

fn duration(months: i32, days: i32, micros: i64) -> ElementProperty {
    ElementProperty::Temporal(Temporal::Duration(Duration::new(months, days, micros)))
}

#[test]
fn duration_between_follows_neo4j_over_mixed_operands() {
    let graph = between_graph();
    for (expr, expected) in [
        // Temporal10 [2]: months, then days, then the rest; a missing date gives the times only.
        ("duration.between(n.l0, n.r0)", duration(368, 13, 0)),
        (
            "duration.between(n.l0, n.r1)",
            duration(381, 10, hms(21, 45, 22_142)),
        ),
        (
            "duration.between(n.l0, n.r3)",
            duration(0, 0, hms(16, 30, 0)),
        ),
        (
            "duration.between(n.l1, n.r0)",
            duration(0, 0, -hms(14, 30, 0)),
        ),
        (
            "duration.between(n.l1, n.r2)",
            duration(0, 0, hms(7, 10, 32_142)),
        ),
        (
            "duration.between(n.l2, n.r0)",
            duration(0, -27, -hms(21, 40, 32_142)),
        ),
        ("duration.between(n.l2, n.r2)", duration(0, 0, 0)),
        (
            "duration.between(n.l3, n.r0)",
            duration(11, 2, hms(2, 19, 23_857)),
        ),
        (
            "duration.between(n.l3, n.r2)",
            duration(12, 0, hms(0, 59, 55_999)),
        ),
        // [3], [4], [5]: one unit each.
        ("duration.inMonths(n.l0, n.r2)", duration(369, 0, 0)),
        ("duration.inMonths(n.l1, n.r1)", duration(0, 0, 0)),
        ("duration.inMonths(n.l3, n.r1)", duration(24, 0, 0)),
        ("duration.inDays(n.l0, n.r1)", duration(0, 11_606, 0)),
        ("duration.inDays(n.l2, n.r0)", duration(0, -27, 0)),
        ("duration.inDays(n.l3, n.r2)", duration(0, 365, 0)),
        (
            "duration.inSeconds(n.l0, n.r2)",
            duration(0, 0, hms(269_781, 40, 32_142)),
        ),
        (
            "duration.inSeconds(n.l2, n.r0)",
            duration(0, 0, -hms(669, 40, 32_142)),
        ),
        (
            "duration.inSeconds(n.l3, n.r3)",
            duration(0, 0, -hms(5, 10, 36_143)),
        ),
        ("duration.inSeconds(n.l1, n.l1)", duration(0, 0, 0)),
        // [7]: negative months against an operand at another offset.
        ("duration.inMonths(n.r2, n.l0)", duration(-369, 0, 0)),
        // NULL in either argument (F2).
        ("duration.between(NULL, NULL)", ElementProperty::Null),
        ("duration.inMonths(n.l0, n.missing)", ElementProperty::Null),
        ("duration.inDays(NULL, 1)", ElementProperty::Null),
        ("duration.inSeconds(n.missing, n.r0)", ElementProperty::Null),
    ] {
        assert_eq!(duration_of(&graph, expr), expected, "{expr}");
    }
    // A non-temporal operand, a duration, and the months or days of two local times: 22G03.
    for expr in [
        "duration.between(n.l0, 1)",
        "duration.between('2020-01-01', n.r0)",
        "duration.inSeconds(n.l0, n.u)",
        "duration.inMonths(n.l1, n.r3)",
        "duration.inDays(n.l1, n.r3)",
    ] {
        let query = format!("MATCH (n:t) RETURN {expr} AS x");
        let (status, kind, message, compile_time) = failure(&graph, &query);
        assert_eq!(status, "22G03", "{query}: {message}");
        assert_eq!(kind, GqlErrorKind::Data, "{query}");
        assert!(!compile_time, "{query}");
        assert!(message.starts_with("duration."), "{query}: {message}");
    }
    // No row reaching the call, no error.
    assert!(
        rows(
            &graph,
            "MATCH (n:nothing) RETURN duration.inDays(n.l1, n.r3) AS x"
        )
        .is_empty()
    );
}
