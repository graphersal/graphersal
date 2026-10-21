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
use graphersal::temporal::{Date, Duration, Temporal, TemporalKind};

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
        ("date.truncate('month', n.d)", "date.truncate()"),
        (
            "localdatetime.truncate('day', n.d)",
            "localdatetime.truncate()",
        ),
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
