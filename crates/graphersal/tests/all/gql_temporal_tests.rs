//! GQL stage 3g, temporal functions (feature `gql`). 3g.T2: the names parse and bind as ordinary
//! functions (ISO `local_time` / `local_datetime` / `zoned_datetime` beside the Cypher names,
//! the qualified Cypher rows `date.truncate`, `duration.between`, ...), temporal values flow
//! through the evaluator (comparison, static kinds that never refuse a temporal operand where a
//! later task gives it a meaning), and every form a later task implements answers 0A000 "not
//! yet" at run time instead of a wrong value. Cypher's zoned `time()` stays "not yet" (U2),
//! `X.realtime()` is refused (U9). 3g.T5: components (`d.year`, `dur.monthsOfYear`) with Neo4j's
//! names and meanings, `toString` in the Neo4j text, `CAST` in the engine text (U6).
#![cfg(feature = "gql")]

use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;
use graphersal::temporal::{Date, DateTime, Duration, LocalDateTime, Temporal, Time};

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

/// One `t` vertex holding a value of every engine temporal type (3g.T5).
fn typed_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let date = Date::from_ymd(1984, 11, 11).expect("a date");
    let time = Time::from_hms_micro(12, 31, 14, 645_876).expect("a time");
    let local = LocalDateTime::new(date, time);
    let zoned = DateTime::from_local(local, 3600).expect("a datetime");
    let noon = Time::from_hms_micro(12, 0, 0, 0).expect("a time");
    let values = [
        ("d", Temporal::Date(date)),
        ("t", Temporal::Time(time)),
        ("l", Temporal::LocalDateTime(local)),
        ("z", Temporal::DateTime(zoned)),
        // 1 year 4 months, 10 days, 1 h 1 min 1.111111 s.
        (
            "u",
            Temporal::Duration(Duration::new(16, 10, 3_661_111_111)),
        ),
        // -1 minute -0.001 seconds.
        ("n", Temporal::Duration(Duration::new(0, 0, -60_001_000))),
        ("h", Temporal::Time(noon)),
    ];
    let mut traversal = graph.traversal_mut().add_v("t");
    for (key, value) in values {
        traversal = traversal.property(key, ElementProperty::Temporal(value));
    }
    traversal.iterate().expect("the vertex");
    graph
}

#[test]
fn components_have_the_neo4j_names_and_meanings() {
    let graph = typed_graph();
    let int = |v: i64| ElementProperty::from(v);
    let text = |v: &str| ElementProperty::String(v.into());
    for (expr, expected) in [
        // Temporal5 [5] / [6] at microsecond precision.
        ("n.d.year", int(1984)),
        ("n.d.quarter", int(4)),
        ("n.d.month", int(11)),
        ("n.d.week", int(45)),
        ("n.d.weekYear", int(1984)),
        ("n.d.day", int(11)),
        ("n.d.ordinalDay", int(316)),
        ("n.d.weekDay", int(7)),
        ("n.d.dayOfWeek", int(7)),
        ("n.d.dayOfQuarter", int(42)),
        ("n.d.quarterDay", int(42)),
        ("n.t.hour", int(12)),
        ("n.t.minute", int(31)),
        ("n.t.second", int(14)),
        ("n.t.millisecond", int(645)),
        ("n.t.microsecond", int(645_876)),
        ("n.t.nanosecond", int(645_876_000)),
        ("n.l.ordinalDay", int(316)),
        ("n.l.microsecond", int(645_876)),
        ("n.z.weekYear", int(1984)),
        ("n.z.millisecond", int(645)),
        ("n.z.timezone", text("+01:00")),
        ("n.z.offset", text("+01:00")),
        ("n.z.offsetMinutes", int(60)),
        ("n.z.offsetSeconds", int(3600)),
        ("n.z.epochSeconds", int(469_020_674)),
        ("n.z.epochMillis", int(469_020_674_645)),
        // Temporal5 [7] at microsecond precision.
        ("n.u.years", int(1)),
        ("n.u.quarters", int(5)),
        ("n.u.months", int(16)),
        ("n.u.weeks", int(1)),
        ("n.u.days", int(10)),
        ("n.u.hours", int(1)),
        ("n.u.minutes", int(61)),
        ("n.u.seconds", int(3661)),
        ("n.u.milliseconds", int(3_661_111)),
        ("n.u.microseconds", int(3_661_111_111)),
        ("n.u.nanoseconds", int(3_661_111_111_000)),
        ("n.u.quartersOfYear", int(1)),
        ("n.u.monthsOfQuarter", int(1)),
        ("n.u.monthsOfYear", int(4)),
        ("n.u.daysOfWeek", int(3)),
        ("n.u.minutesOfHour", int(1)),
        ("n.u.secondsOfMinute", int(1)),
        ("n.u.millisecondsOfSecond", int(111)),
        ("n.u.microsecondsOfSecond", int(111_111)),
        ("n.u.nanosecondsOfSecond", int(111_111_000)),
        // Neo4j's normalized seconds: rounded down, a non-negative fraction.
        ("n.n.seconds", int(-61)),
        ("n.n.millisecondsOfSecond", int(999)),
        ("n.n.milliseconds", int(-60_001)),
        // Any ASCII case, as in Neo4j; NULL stays NULL.
        ("n.d.WEEKYEAR", int(1984)),
        ("n.missing.year", ElementProperty::Null),
    ] {
        let query = format!("MATCH (n:t) RETURN {expr} AS x");
        assert_eq!(single(&graph, &query), expected, "{query}");
    }
}

#[test]
fn an_unknown_component_names_the_components_of_the_kind() {
    let graph = typed_graph();
    for (expr, needle) in [
        (
            "n.d.hour",
            "a date has no component hour; its components are year, quarter",
        ),
        (
            "n.t.year",
            "a local time has no component year; its components are hour",
        ),
        (
            "n.l.offset",
            "a local datetime has no component offset; its components are year",
        ),
        (
            "n.z.years",
            "a datetime has no component years; its components are year",
        ),
        (
            "n.u.year",
            "a duration has no component year; its components are years",
        ),
        ("n.d.nope", "a date has no component nope"),
    ] {
        let query = format!("MATCH (n:t) RETURN {expr} AS x");
        let (status, kind, message, compile_time) = failure(&graph, &query);
        assert_eq!(status, "22G03", "{query}");
        assert_eq!(kind, GqlErrorKind::Data, "{query}");
        assert!(!compile_time, "{query}");
        assert!(message.starts_with(needle), "{query}: {message}");
    }
    // A total beyond the integer range is 22003.
    let mut graph = TraversalGraph::new();
    let long = Temporal::Duration(Duration::new(0, 0, i64::MAX));
    graph
        .traversal_mut()
        .add_v("t")
        .property("u", ElementProperty::Temporal(long))
        .iterate()
        .expect("the vertex");
    let (status, ..) = failure(&graph, "MATCH (n:t) RETURN n.u.nanoseconds AS x");
    assert_eq!(status, "22003");
}

#[test]
fn to_string_writes_the_neo4j_text_and_cast_the_engine_text() {
    let graph = typed_graph();
    let text = |v: &str| ElementProperty::String(v.into());
    for (key, neo4j, engine) in [
        ("d", "1984-11-11", "1984-11-11"),
        ("h", "12:00", "12:00:00"),
        ("t", "12:31:14.645876", "12:31:14.645876"),
        (
            "l",
            "1984-11-11T12:31:14.645876",
            "1984-11-11T12:31:14.645876",
        ),
        (
            "z",
            "1984-11-11T12:31:14.645876+01:00",
            "1984-11-11T12:31:14.645876+01:00",
        ),
        ("n", "PT-1M-0.001S", "-PT1M0.001S"),
    ] {
        for (call, expected) in [
            (format!("toString(n.{key})"), neo4j),
            (format!("toStringOrNull(n.{key})"), neo4j),
            (format!("CAST(n.{key} AS STRING)"), engine),
        ] {
            let query = format!("MATCH (n:t) RETURN {call} AS x");
            assert_eq!(single(&graph, &query), text(expected), "{query}");
        }
    }
    // CAST of a list keeps the engine text of its temporal items.
    assert_eq!(
        single(&graph, "MATCH (n:t) RETURN CAST([n.h] AS STRING) AS x"),
        text("[12:00:00]")
    );
    assert_eq!(
        single(&graph, "MATCH (n:t) RETURN toString(n.missing) AS x"),
        ElementProperty::Null
    );
}
