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
use graphersal::temporal::{Date, DateTime, Duration, LocalDateTime, Temporal, TemporalKind, Time};

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
        "datetime.truncate('day', n.d).hour",
        "duration.between(n.d, n.d).days",
    ] {
        let none = format!("MATCH (n:nothing) RETURN {expr} AS x");
        assert!(rows(&graph, &none).is_empty(), "{none}");
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        assert_ne!(single(&graph, &query), ElementProperty::Null, "{query}");
    }
    for expr in [
        "date() - duration('P1D')",
        "duration('P1D') * 2",
        "duration('P1D') / 2",
        "-duration('P1D')",
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

// ----- 3g.T4: constructors, parsing, clocks -----

/// The engine text of the temporal value `x` of the single row of `query`.
fn temporal_text(graph: &TraversalGraph, query: &str) -> String {
    match single(graph, query) {
        ElementProperty::Temporal(value) => value.to_string(),
        other => panic!("{query}: not a temporal value: {other:?}"),
    }
}

/// The column `key` of a result row.
fn column(row: &ElementProperty, key: &str) -> ElementProperty {
    match row {
        ElementProperty::Map(map) => map.get(&ElementProperty::from(key)).cloned(),
        ElementProperty::Object(map) => map.get(key).cloned(),
        _ => None,
    }
    .unwrap_or_else(|| panic!("no column {key} in {row:?}"))
}

/// `text` read as a value of `kind` by the engine's own parser (the Rhai literal's), as text.
fn engine_text(kind: TemporalKind, text: &str) -> String {
    Temporal::parse_as(kind, text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
        .to_string()
}

#[test]
fn map_constructors_build_every_form() {
    let graph = graph();
    use TemporalKind::{Date as D, DateTime as Z, Duration as U, LocalDateTime as L, Time as T};
    for (expr, kind, expected) in [
        // Calendar, week, ordinal and quarter dates (ISO weeks: 1817 week 1 starts 1816-12-30).
        ("date({year: 1984, month: 10, day: 11})", D, "1984-10-11"),
        ("date({year: 1984, month: 10})", D, "1984-10-01"),
        ("date({year: 1984})", D, "1984-01-01"),
        ("date({year: 1817, week: 1})", D, "1816-12-30"),
        ("date({year: 1818, week: 53})", D, "1818-12-28"),
        ("date({dayOfWeek: 2, year: 1817, week: 1})", D, "1816-12-31"),
        (
            "date({year: 1984, week: 10, dayOfWeek: 3})",
            D,
            "1984-03-07",
        ),
        ("date({year: 1984, ordinalDay: 202})", D, "1984-07-20"),
        (
            "date({year: 1984, quarter: 3, dayOfQuarter: 45})",
            D,
            "1984-08-14",
        ),
        ("date({year: 1984, quarter: 3})", D, "1984-07-01"),
        // Keys are case-insensitive.
        ("date({YEAR: 1984, Month: 10, DAY: 11})", D, "1984-10-11"),
        // Projections with overrides (`java.time` `with`).
        (
            "date({date: date('1816-12-30'), week: 2, dayOfWeek: 3})",
            D,
            "1817-01-08",
        ),
        ("date({date: date('1816-12-31'), week: 2})", D, "1817-01-07"),
        (
            "date({date: date('1816-12-31'), year: 1817, week: 2})",
            D,
            "1817-01-07",
        ),
        (
            "date({date: date('1984-11-11'), year: 28})",
            D,
            "0028-11-11",
        ),
        ("date({date: date('1984-11-11'), week: 1})", D, "1984-01-08"),
        (
            "date({date: date('1984-11-11'), ordinalDay: 28})",
            D,
            "1984-01-28",
        ),
        (
            "date({date: date('1984-11-11'), quarter: 3})",
            D,
            "1984-08-11",
        ),
        (
            "date({date: date('2020-02-29'), year: 2021})",
            D,
            "2021-02-28",
        ),
        (
            "date({date: datetime('1984-11-11T23:00-05:00')})",
            D,
            "1984-11-11",
        ),
        // Times: millisecond + microsecond add up.
        (
            "localtime({hour: 12, minute: 31, second: 14, millisecond: 123, microsecond: 456})",
            T,
            "12:31:14.123456",
        ),
        (
            "local_time({hour: 12, minute: 31, second: 14, microsecond: 645876})",
            T,
            "12:31:14.645876",
        ),
        ("localtime({hour: 12})", T, "12:00"),
        (
            "localtime({time: localtime('12:31:14.645'), second: 42})",
            T,
            "12:31:42.645",
        ),
        (
            "localtime({time: datetime('1984-10-11T12:00+01:00')})",
            T,
            "12:00",
        ),
        // Local datetimes.
        (
            "localdatetime({year: 1984, week: 10, dayOfWeek: 3, hour: 12, minute: 31})",
            L,
            "1984-03-07T12:31",
        ),
        ("local_datetime({year: 1984})", L, "1984-01-01T00:00"),
        (
            "localdatetime({date: date('1984-10-11'), time: localtime('12:31:14.645'), day: 28, second: 42})",
            L,
            "1984-10-28T12:31:42.645",
        ),
        (
            "localdatetime({datetime: datetime('1984-10-11T12:00+01:00'), day: 28, second: 42})",
            L,
            "1984-10-28T12:00:42",
        ),
        // Datetimes: UTC by default, an offset, a zone name resolved to its offset then.
        (
            "datetime({year: 1984, month: 10, day: 11, hour: 12})",
            Z,
            "1984-10-11T12:00Z",
        ),
        (
            "zoned_datetime({year: 1984, month: 10, day: 11, hour: 12, timezone: '+01:00'})",
            Z,
            "1984-10-11T12:00+01:00",
        ),
        (
            "datetime({year: 1984, ordinalDay: 202, hour: 12, timezone: 'Europe/Stockholm'})",
            Z,
            "1984-07-20T12:00+02:00",
        ),
        (
            "datetime({year: 1984, month: 10, day: 11, hour: 12, minute: 34, second: 56, timezone: '+02:05:59'})",
            Z,
            "1984-10-11T12:34:56+02:05:59",
        ),
        // A base with an offset converts to the timezone; a local one is placed in it.
        (
            "datetime({datetime: datetime('1984-10-11T12:00+01:00'), timezone: '+05:00'})",
            Z,
            "1984-10-11T16:00+05:00",
        ),
        (
            "datetime({datetime: localdatetime('1984-03-07T12:31:14.645'), timezone: '+05:00'})",
            Z,
            "1984-03-07T12:31:14.645+05:00",
        ),
        (
            "datetime({date: date('1984-10-11'), time: datetime('1984-10-11T12:00+01:00'), second: 42})",
            Z,
            "1984-10-11T12:00:42+01:00",
        ),
        (
            "datetime({date: datetime('1984-10-11T12:00+01:00'), time: localtime('12:31')})",
            Z,
            "1984-10-11T12:31Z",
        ),
        // Durations: fractions cascade (a month is 30.436875 days).
        (
            "duration({days: 14, hours: 16, minutes: 12})",
            U,
            "P14DT16H12M",
        ),
        ("duration({months: 5, days: 1.5})", U, "P5M1DT12H"),
        ("duration({months: 0.75})", U, "P22DT19H51M49.5S"),
        ("duration({weeks: 2.5})", U, "P17DT12H"),
        (
            "duration({years: 12, months: 5, days: 14, hours: 16, minutes: 12, seconds: 70})",
            U,
            "P12Y5M14DT16H13M10S",
        ),
        (
            "duration({days: 14, seconds: 70, microseconds: 1})",
            U,
            "P14DT1M10.000001S",
        ),
        ("duration({minutes: 1.5, seconds: 1})", U, "PT1M31S"),
        ("duration({})", U, "PT0S"),
        // From the epoch.
        (
            "datetime.fromepochmillis(237821673987)",
            Z,
            "1977-07-15T13:34:33.987Z",
        ),
        (
            "datetime.fromepoch(416779, 999999000)",
            Z,
            "1970-01-05T19:46:19.999999Z",
        ),
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        assert_eq!(
            temporal_text(&graph, &query),
            engine_text(kind, expected),
            "{query}"
        );
    }
}

#[test]
fn string_constructors_read_iso_8601_in_full() {
    let graph = graph();
    use TemporalKind::{Date as D, DateTime as Z, Duration as U, LocalDateTime as L, Time as T};
    for (text, function, kind, expected) in [
        ("2015-07-21", "date", D, "2015-07-21"),
        ("20150721", "date", D, "2015-07-21"),
        ("201507", "date", D, "2015-07-01"),
        ("2015-W30-2", "date", D, "2015-07-21"),
        ("2015W30", "date", D, "2015-07-20"),
        ("2015-202", "date", D, "2015-07-21"),
        ("2015", "date", D, "2015-01-01"),
        ("214032.142", "localtime", T, "21:40:32.142"),
        ("2140", "local_time", T, "21:40"),
        ("21", "localtime", T, "21:00"),
        (
            "2015-W30-2T214032.142",
            "localdatetime",
            L,
            "2015-07-21T21:40:32.142",
        ),
        ("2015202T21", "local_datetime", L, "2015-07-21T21:00"),
        (
            "2015-07-21T21:40:32.142+0100",
            "datetime",
            Z,
            "2015-07-21T21:40:32.142+01:00",
        ),
        ("2015-W30T2140-02", "datetime", Z, "2015-07-20T21:40-02:00"),
        (
            "2015202T21+18:00",
            "zoned_datetime",
            Z,
            "2015-07-21T21:00+18:00",
        ),
        ("2015-07-21T21:40", "datetime", Z, "2015-07-21T21:40Z"),
        // A zone name resolves to its offset at that time (the value keeps the offset, U3).
        (
            "2015-07-21T21:40:32.142[Europe/London]",
            "datetime",
            Z,
            "2015-07-21T21:40:32.142+01:00",
        ),
        ("P5M1.5D", "duration", U, "P5M1DT12H"),
        ("PT0.75M", "duration", U, "PT45S"),
        (
            "P2012-02-02T14:37:21.545",
            "duration",
            U,
            "P2012Y2M2DT14H37M21.545S",
        ),
    ] {
        let query = format!("MATCH (n:x) RETURN {function}('{text}') AS x");
        assert_eq!(
            temporal_text(&graph, &query),
            engine_text(kind, expected),
            "{query}"
        );
    }
}

#[test]
fn temporal_values_project_onto_other_types() {
    let graph = graph();
    use TemporalKind::{Date as D, DateTime as Z, LocalDateTime as L, Time as T};
    for (expr, kind, expected) in [
        ("date(datetime('1984-11-11T23:30-05:00'))", D, "1984-11-11"),
        ("date(localdatetime('1984-11-11T23:30'))", D, "1984-11-11"),
        ("date(n.d)", D, "2020-01-01"),
        ("localtime(datetime('1984-10-11T12:00+01:00'))", T, "12:00"),
        (
            "localdatetime(datetime('1984-10-11T12:00+01:00'))",
            L,
            "1984-10-11T12:00",
        ),
        (
            "datetime(localdatetime('1984-10-11T12:00'))",
            Z,
            "1984-10-11T12:00Z",
        ),
        (
            "datetime(datetime('1984-10-11T12:00+01:00'))",
            Z,
            "1984-10-11T12:00+01:00",
        ),
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        assert_eq!(
            temporal_text(&graph, &query),
            engine_text(kind, expected),
            "{query}"
        );
    }
    for expr in [
        "date(localtime('12:00'))",
        "localtime(n.d)",
        "datetime(n.d)",
        "localdatetime(n.u)",
        "date(1)",
        "duration(n.d)",
        "date({date: localtime('12:00')})",
        "date({year: 1984.5})",
        "datetime({year: 1984, timezone: 1})",
        "duration({days: 'x'})",
        "datetime.fromepochmillis(1.5)",
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        let (status, kind, _, compile_time) = failure(&graph, &query);
        assert_eq!(status, "22G03", "{query}");
        assert_eq!(kind, GqlErrorKind::Data, "{query}");
        assert!(!compile_time, "{query}");
    }
}

#[test]
fn bad_maps_and_text_are_22007_and_out_of_range_fields_22008() {
    let graph = graph();
    for (expr, status, needle) in [
        ("date('2015-02-30x')", "22007", "cannot parse"),
        ("localtime('12:00+01:00')", "22007", "no UTC offset"),
        (
            "date({year: 1984, colour: 1})",
            "22007",
            "unknown field colour",
        ),
        (
            "date({year: 1984, hour: 1})",
            "22007",
            "cannot have the field hour",
        ),
        (
            "localtime({hour: 1, timezone: '+01:00'})",
            "22007",
            "cannot have the field timezone",
        ),
        (
            "date({year: 1984, month: 1, week: 1})",
            "22007",
            "one of month / day",
        ),
        ("date({month: 1})", "22007", "needs the field year"),
        (
            "date({year: 1984, day: 3})",
            "22007",
            "needs the field month",
        ),
        ("localtime({minute: 1})", "22007", "needs the field hour"),
        (
            "localtime({hour: 1, second: 1})",
            "22007",
            "needs the field minute",
        ),
        ("date({})", "22007", "needs the field year or date"),
        (
            "datetime({year: 1984, timezone: 'Mars/Olympus'})",
            "22007",
            "unknown time zone",
        ),
        (
            "datetime({datetime: localdatetime('1984-01-01T00:00'), date: n.d})",
            "22007",
            "cannot be combined",
        ),
        (
            "duration({fortnights: 1})",
            "22007",
            "unknown field fortnights",
        ),
        (
            "date({year: 1984, month: 13})",
            "22008",
            "month cannot be 13",
        ),
        (
            "date({year: 1983, month: 2, day: 29})",
            "22008",
            "day cannot be 29",
        ),
        ("date({year: 1984, week: 53})", "22008", "week cannot be 53"),
        (
            "date({year: 1984, ordinalDay: 367})",
            "22008",
            "ordinalDay cannot be 367",
        ),
        ("date({year: 10000})", "22008", "year cannot be 10000"),
        ("localtime({hour: 24})", "22008", "hour cannot be 24"),
        (
            "localtime({hour: 1, minute: 1, second: 1, millisecond: 1, microsecond: 1000})",
            "22008",
            "microsecond cannot be 1000",
        ),
        (
            "date('+12015-01-01')",
            "22008",
            "outside the supported range",
        ),
        (
            "datetime.fromepochmillis(9223372036854775807)",
            "22008",
            "outside the supported range",
        ),
        (
            "datetime.fromepoch(9223372036854775807, 0)",
            "22008",
            "outside the supported range",
        ),
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        let (actual, kind, message, compile_time) = failure(&graph, &query);
        assert_eq!(actual, status, "{query}: {message}");
        assert_eq!(kind, GqlErrorKind::Data, "{query}");
        assert!(!compile_time, "{query}");
        assert!(message.contains(needle), "{query}: {message}");
    }
}

#[test]
fn below_the_microsecond_is_not_yet_never_cut() {
    let graph = graph();
    for expr in [
        "localtime({hour: 12, minute: 31, second: 14, nanosecond: 645876123})",
        "localtime({hour: 12, minute: 31, second: 14, millisecond: 1, microsecond: 2, nanosecond: 3})",
        "localtime('12:31:14.645876123')",
        "datetime('2015-07-21T21:40:32.0000001Z')",
        "duration({nanoseconds: 1})",
        "duration('PT0.0000001S')",
        "duration({seconds: 0.0000001})",
        "datetime.fromepoch(416779, 999999999)",
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        let (status, kind, message, compile_time) = failure(&graph, &query);
        assert_eq!(status, "0A000", "{query}");
        assert_eq!(kind, GqlErrorKind::NotYet, "{query}");
        assert!(!compile_time, "{query}");
        assert!(
            message.contains("below the microsecond"),
            "{query}: {message}"
        );
    }
    // Whole microseconds written with nine digits are kept.
    assert_eq!(
        temporal_text(
            &graph,
            "RETURN localtime({hour: 1, minute: 0, second: 0, nanosecond: 5000}) AS x"
        ),
        engine_text(TemporalKind::Time, "01:00:00.000005")
    );
}

#[test]
fn null_arguments_and_fields_give_null() {
    let graph = graph();
    for expr in [
        "date(NULL)",
        "localtime(NULL)",
        "local_datetime(NULL)",
        "datetime(NULL)",
        "duration(NULL)",
        "date.statement(NULL)",
        "datetime.transaction(NULL)",
        "localtime.statement(NULL)",
        "localdatetime.transaction(NULL)",
        "datetime.fromepoch(NULL, 0)",
        "datetime.fromepoch(0, NULL)",
        "datetime.fromepochmillis(NULL)",
        "date({year: n.missing})",
        "datetime({year: 1984, timezone: NULL})",
        "duration({days: n.missing})",
        "date({timezone: NULL})",
        "datetime.statement({timezone: NULL})",
    ] {
        let query = format!("MATCH (n:x) RETURN {expr} AS x");
        assert_eq!(single(&graph, &query), ElementProperty::Null, "{query}");
    }
}

#[test]
fn clocks_read_the_one_pinned_time_of_the_execution() {
    let graph = graph();
    // Every clock of one execution reads the same instant, in every row.
    let query = "UNWIND [1, 2, 3] AS i RETURN datetime() AS a, datetime.statement() AS b, \
                 datetime.transaction() AS c, localdatetime() AS d, date() AS e, \
                 localtime() AS f, zoned_datetime() AS g";
    let rows = rows(&graph, query);
    assert_eq!(rows.len(), 3);
    let first = &rows[0];
    for row in &rows {
        assert_eq!(row, first, "{rows:?}");
    }
    let value = |key: &str| match column(first, key) {
        ElementProperty::Temporal(t) => t,
        other => panic!("{key}: {other:?}"),
    };
    let Temporal::DateTime(now) = value("a") else {
        panic!("{first:?}");
    };
    assert_eq!(now.offset_seconds(), 0, "UTC by default");
    assert_eq!(value("b"), value("a"));
    assert_eq!(value("c"), value("a"));
    assert_eq!(value("g"), value("a"));
    assert_eq!(value("d"), Temporal::LocalDateTime(now.local()));
    assert_eq!(value("e"), Temporal::Date(now.local().date()));
    assert_eq!(value("f"), Temporal::Time(now.local().time()));
    // A pinned time (`with_now`) is the clocks' time; a zone gives its wall clock.
    let at: graphersal::temporal::DateTime = "2024-03-31T00:30:00Z".parse().expect("a datetime");
    let pinned = graph
        .traversal()
        .with_now(at)
        .gql(
            "RETURN datetime() AS a, datetime.statement('Europe/Berlin') AS b, \
             date({timezone: 'Pacific/Kiritimati'}) AS c, localtime.transaction({timezone: '-05:00'}) AS d",
        )
        .to_list()
        .expect("the clocks");
    let [row] = pinned.as_slice() else {
        panic!("{pinned:?}");
    };
    let text = |key: &str| match column(row, key) {
        ElementProperty::Temporal(t) => t.to_string(),
        other => panic!("{key}: {other:?}"),
    };
    assert_eq!(
        text("a"),
        engine_text(TemporalKind::DateTime, "2024-03-31T00:30:00Z")
    );
    assert_eq!(
        text("b"),
        engine_text(TemporalKind::DateTime, "2024-03-31T01:30:00+01:00")
    );
    assert_eq!(text("c"), engine_text(TemporalKind::Date, "2024-03-31"));
    assert_eq!(text("d"), engine_text(TemporalKind::Time, "19:30:00"));
}

#[test]
fn constructed_values_are_stored_by_insert_and_set() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .gql(
            "INSERT (:e {d: date({year: 1984, month: 10, day: 11}), \
             l: [localtime({hour: 13}), localtime({hour: 14})], \
             u: duration({days: 14, hours: 16, minutes: 12})})",
        )
        .iterate()
        .expect("the insert");
    graph
        .traversal_mut()
        .gql("MATCH (n:e) SET n.t = datetime({year: 1984, month: 10, day: 11, timezone: '+01:00'})")
        .iterate()
        .expect("the set");
    let row = rows(
        &graph,
        "MATCH (n:e) RETURN n.d AS d, n.l AS l, n.u AS u, n.t AS t",
    );
    let [row] = row.as_slice() else {
        panic!("{row:?}");
    };
    let text = |value: &ElementProperty| match value {
        ElementProperty::Temporal(t) => t.to_string(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        text(&column(row, "d")),
        engine_text(TemporalKind::Date, "1984-10-11")
    );
    assert_eq!(
        text(&column(row, "u")),
        engine_text(TemporalKind::Duration, "P14DT16H12M")
    );
    assert_eq!(
        text(&column(row, "t")),
        engine_text(TemporalKind::DateTime, "1984-10-11T00:00+01:00")
    );
    let ElementProperty::Array(items) = column(row, "l") else {
        panic!("{row:?}");
    };
    let items: Vec<String> = items.iter().map(text).collect();
    assert_eq!(
        items,
        [
            engine_text(TemporalKind::Time, "13:00"),
            engine_text(TemporalKind::Time, "14:00")
        ]
    );
    // The stored values compare with constructed ones.
    assert_eq!(
        single(&graph, "MATCH (n:e) RETURN n.d = date('1984-10-11') AS x"),
        ElementProperty::Boolean(true)
    );
}

/// Twins: a GQL constructor gives the value of the Rhai literal of the same text.
#[cfg(feature = "script")]
#[test]
fn constructors_equal_the_script_literals() {
    use std::sync::Arc;
    for (gql, rhai) in [
        (
            "date({year: 1984, month: 10, day: 11})",
            r#"date("1984-10-11")"#,
        ),
        ("date('2015-W30-2')", r#"date("2015-07-21")"#),
        (
            "localtime({hour: 12, minute: 31, second: 14, millisecond: 645})",
            r#"time("12:31:14.645")"#,
        ),
        (
            "localdatetime('2015-07-21T21:40:32.142')",
            r#"localdatetime("2015-07-21T21:40:32.142")"#,
        ),
        (
            "datetime({year: 1984, month: 10, day: 11, hour: 12, timezone: '+01:00'})",
            r#"datetime("1984-10-11T12:00:00+01:00")"#,
        ),
        (
            "datetime('2015-07-21T21:40:32.142+0100')",
            r#"datetime("2015-07-21T21:40:32.142+01:00")"#,
        ),
        (
            "duration({days: 14, hours: 16, minutes: 12})",
            r#"duration("P14DT16H12M")"#,
        ),
        (
            "duration('P12Y5M14DT16H12M70S')",
            r#"duration("P12Y5M14DT16H13M10S")"#,
        ),
        (
            "datetime.fromepochmillis(237821673987)",
            r#"datetime("1977-07-15T13:34:33.987Z")"#,
        ),
    ] {
        let script = format!(
            r#"let v = g.gql("RETURN {gql} AS x").next()["x"]; let w = {rhai}; v == w && v.as_string() == w.as_string()"#
        );
        let result = crate::script_host::eval_value(Arc::new(GraphSource::empty()), &script)
            .unwrap_or_else(|e| panic!("{script}: {e}"));
        assert_eq!(result.as_bool().ok(), Some(true), "{script}");
    }
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

/// 3g.T7: a vertex `t` holding one value of each type: `d` = 1984-10-11, `l` =
/// 1984-10-11T12:31:14.645876, `z` = the same wall clock at -01:00, `t` = 12:31:14.645876, `y` =
/// 1984-01-01 (ISO week-year 1983), `u` = a duration.
fn truncation_graph() -> TraversalGraph {
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
