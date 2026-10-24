//! The date steps and the script's date and time values (book part "Dates and Times"):
//! `as_date`, `as_duration`, `date_add`, `date_diff`, `age`, `date_trunc`, `date_part`, `at_zone`,
//! `as_local` in both spellings and through the Rust API, the `DT` token in every notation, the
//! constructors, getters and operators of the five script types, `now()`/`today()` (one value per
//! evaluation, pinnable), predicates, ordering and `typeOf`, the profile names and the camelCase
//! spelling of the diagnostics. The `help()` texts are in `temporal_error_tests.rs`.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;
use graphersal::temporal::{DT, DateTime, Duration, Temporal};

use crate::script_host::{debug_value, eval_value};

/// Runs a script on an empty graph and renders its value (a date as its DSL literal).
fn run(script: &str) -> String {
    run_on(Arc::new(GraphSource::empty()), script)
}

fn run_on(graph: Arc<Graph>, script: &str) -> String {
    match eval_value(graph, script) {
        Ok(value) => debug_value(&value),
        Err(err) => panic!("{script}: {err}"),
    }
}

/// The error text of a failing script.
fn fails(script: &str) -> String {
    match eval_value(Arc::new(GraphSource::empty()), script) {
        Ok(value) => panic!("{script} must fail, got {}", debug_value(&value)),
        Err(err) => err.to_string(),
    }
}

/// Both spellings of a script give the same rendering.
fn both(snake: &str, camel: &str) -> String {
    let a = run(snake);
    assert_eq!(a, run(camel), "{snake} vs {camel}");
    a
}

fn dt(text: &str) -> DateTime {
    text.parse().unwrap()
}

fn temporal_text(value: &ElementProperty) -> String {
    value
        .as_temporal()
        .unwrap_or_else(|| panic!("not a temporal value: {value:?}"))
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// as_date / as_duration
// ---------------------------------------------------------------------------------------------

#[test]
fn as_date_is_tinkerpops() {
    assert_eq!(
        both(
            r#"g.inject("2023-08-02T00:00:00Z", "2023-08-02T00:00:00-07:00", "2023-08-02", 1694017707000).as_date().to_list()"#,
            r#"g.inject("2023-08-02T00:00:00Z", "2023-08-02T00:00:00-07:00", "2023-08-02", 1694017707000).asDate().toList()"#,
        ),
        r#"[datetime("2023-08-02T00:00:00Z"), datetime("2023-08-02T00:00:00-07:00"), datetime("2023-08-02T00:00:00Z"), datetime("2023-09-06T16:28:27Z")]"#
    );
    // Lenient: a text without an offset is UTC.
    assert_eq!(
        run(r#"g.inject("2023-08-02T10:00").as_date().next()"#),
        r#"datetime("2023-08-02T10:00:00Z")"#
    );
    // A datetime passes; a date and a localdatetime become the datetime at UTC.
    assert_eq!(
        run(
            r#"g.inject(datetime("2023-08-02T00:00:00+02:00"), date("2023-08-02"), localdatetime("2023-08-02T09:30")).as_date().to_list()"#
        ),
        r#"[datetime("2023-08-02T00:00:00+02:00"), datetime("2023-08-02T00:00:00Z"), datetime("2023-08-02T09:30:00Z")]"#
    );
    for bad in [
        "1694017709000.1",
        "[1, 2]",
        "()",
        r#""This String is not an ISO 8601 Date""#,
        "time(\"10:00\")",
        "true",
    ] {
        let err = fails(&format!("g.inject({bad}).as_date().to_list()"));
        assert!(err.contains("Can't parse"), "{bad}: {err}");
    }
}

#[test]
fn as_date_round_trips_with_as_number() {
    // TinkerPop `g_V_valuesXbirthdayX_asDate_asNumber_asDate`.
    let graph = Arc::new(GraphSource::empty());
    run_on(
        graph.clone(),
        r#"g.add_v("person").property("birthday", "2020-08-02").iterate()"#,
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().values("birthday").as_date().as_number().to_list()"#
        ),
        "[1596326400000]"
    );
    assert_eq!(
        run_on(
            graph,
            r#"g.v().values("birthday").as_date().as_number().as_date().to_list()"#
        ),
        r#"[datetime("2020-08-02T00:00:00Z")]"#
    );
}

#[test]
fn as_date_with_a_gtype_converts_to_that_type() {
    assert_eq!(
        both(
            r#"g.inject("2023-08-02", datetime("2023-08-02T23:30:00-07:00"), localdatetime("2023-08-02T09:30")).as_date(GType.DATE).to_list()"#,
            r#"g.inject("2023-08-02", datetime("2023-08-02T23:30:00-07:00"), localdatetime("2023-08-02T09:30")).asDate(GType.DATE).toList()"#,
        ),
        r#"[date("2023-08-02"), date("2023-08-02"), date("2023-08-02")]"#
    );
    assert_eq!(
        run(
            r#"g.inject("2023-08-02T09:30", date("2023-08-02"), datetime("2023-08-02T09:30:00+02:00")).as_date(GType.LOCALDATETIME).to_list()"#
        ),
        r#"[localdatetime("2023-08-02T09:30:00"), localdatetime("2023-08-02T00:00:00"), localdatetime("2023-08-02T09:30:00")]"#
    );
    assert_eq!(
        run(
            r#"g.inject("09:30", datetime("2023-08-02T09:30:00+02:00")).as_date(GType.TIME).to_list()"#
        ),
        r#"[time("09:30:00"), time("09:30:00")]"#
    );
    assert_eq!(
        run(r#"g.inject(1694017707000).as_date(GType.DATETIME).next()"#),
        r#"datetime("2023-09-06T16:28:27Z")"#
    );
    // Strict per type: a date-time text is not a date, a number converts to a datetime only.
    for (value, target) in [
        (r#""2023-08-02T00:00:00Z""#, "GType.DATE"),
        ("1694017707000", "GType.DATE"),
        (r#""09:30+02:00""#, "GType.TIME"),
        (r#"time("09:30")"#, "GType.DATE"),
    ] {
        let err = fails(&format!("g.inject({value}).as_date({target}).to_list()"));
        assert!(err.contains("Can't parse"), "{value} {target}: {err}");
    }
    // A token that names no date or time type fails before anything runs.
    for target in ["GType.STRING", "GType.DURATION"] {
        let err = fails(&format!("g.inject().as_date({target}).to_list()"));
        assert!(err.contains("as_duration()"), "{target}: {err}");
    }
}

#[test]
fn as_duration_reads_iso_8601_durations() {
    assert_eq!(
        both(
            r#"g.inject("P1Y2M3DT4H", "-PT1.5S", duration("P2W")).as_duration().to_list()"#,
            r#"g.inject("P1Y2M3DT4H", "-PT1.5S", duration("P2W")).asDuration().toList()"#,
        ),
        r#"[duration("P1Y2M3DT4H"), duration("-PT1.500S"), duration("P14D")]"#
    );
    for bad in [r#""1 day""#, "5", "()"] {
        let err = fails(&format!("g.inject({bad}).as_duration().to_list()"));
        assert!(err.contains("Can't parse"), "{bad}: {err}");
    }
}

#[test]
fn a_failing_date_step_is_not_skipped_by_a_following_count() {
    let err = fails(r#"g.inject("not a date").as_date().count().next()"#);
    assert!(err.contains("Can't parse"), "{err}");
    let err = fails(r#"g.inject(5).date_add(DT.day, 1).count().next()"#);
    assert!(err.contains("date_add"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// date_add
// ---------------------------------------------------------------------------------------------

#[test]
fn date_add_in_every_notation() {
    let expected = r#"[datetime("2023-08-02T02:00:00Z"), datetime("2023-08-02T02:00:00Z")]"#;
    for script in [
        r#"g.inject(datetime("2023-08-02T00:00:00Z"), DateTime("2023-08-02T00:00:00Z")).dateAdd(DT.hour, 2).toList()"#,
        r#"g.inject(datetime("2023-08-02T00:00:00Z"), DateTime("2023-08-02T00:00:00Z")).date_add(DT::hour, 2).to_list()"#,
        r#"g.inject(datetime("2023-08-02T00:00:00Z"), DateTime("2023-08-02T00:00:00Z")).dateAdd(hour, 2).toList()"#,
        r#"g.inject(datetime("2023-08-02T00:00:00Z"), DateTime("2023-08-02T00:00:00Z")).date_add(DT.hour, 2).to_list()"#,
    ] {
        assert_eq!(run(script), expected, "{script}");
    }
    // A bare unit is a shadowable constant.
    assert_eq!(
        run(
            r#"let hour = DT.day; g.inject(datetime("2023-08-02T00:00:00Z")).date_add(hour, 1).next()"#
        ),
        r#"datetime("2023-08-03T00:00:00Z")"#
    );
    assert_eq!(
        run("[second, minute, hour, day]"),
        "[DT.second, DT.minute, DT.hour, DT.day]"
    );
    assert_eq!(run("DT.dayOfWeek == DT::day_of_week"), "true");
}

#[test]
fn date_add_on_every_type() {
    let cases = [
        (
            r#"datetime("2023-08-02T00:00:00-07:00")"#,
            "DT.minute, -90",
            r#"datetime("2023-08-01T22:30:00-07:00")"#,
        ),
        (
            r#"datetime("2023-01-31T10:00:00Z")"#,
            "DT.month, 1",
            r#"datetime("2023-02-28T10:00:00Z")"#,
        ),
        (
            r#"date("2024-01-31")"#,
            "DT.month, 1",
            r#"date("2024-02-29")"#,
        ),
        (
            r#"date("2024-02-29")"#,
            "DT.year, 1",
            r#"date("2025-02-28")"#,
        ),
        (
            r#"date("2023-08-02")"#,
            "DT.week, -1",
            r#"date("2023-07-26")"#,
        ),
        (
            r#"localdatetime("2023-08-02T23:00")"#,
            "DT.hour, 2",
            r#"localdatetime("2023-08-03T01:00:00")"#,
        ),
        (r#"time("23:30")"#, "DT.hour, 1", r#"time("00:30:00")"#),
        (r#"duration("P1D")"#, "DT.month, 1", r#"duration("P1M1D")"#),
    ];
    for (value, args, expected) in cases {
        assert_eq!(
            run(&format!("g.inject({value}).date_add({args}).next()")),
            expected,
            "{value} + {args}"
        );
    }
    // A unit a type does not have.
    let err = fails(r#"g.inject(date("2023-08-02")).date_add(DT.hour, 1).next()"#);
    assert!(err.contains("not defined for a date"), "{err}");
    let err =
        fails(r#"g.inject(datetime("2023-08-02T00:00:00Z")).date_add(DT.day_of_week, 1).next()"#);
    assert!(err.contains("day_of_week"), "{err}");
    // Out of range.
    let err = fails(r#"g.inject(datetime("9999-12-31T00:00:00Z")).date_add(DT.day, 1).next()"#);
    assert!(err.contains("supported range"), "{err}");
}

#[test]
fn date_add_in_a_zone_keeps_the_wall_clock_across_daylight_saving() {
    // Berlin springs forward on 2023-03-26 at 02:00.
    let start = r#"datetime("2023-03-25T12:00:00+01:00")"#;
    assert_eq!(
        run(&format!(
            r#"g.inject({start}).date_add(DT.day, 1, "Europe/Berlin").next()"#
        )),
        r#"datetime("2023-03-26T12:00:00+02:00[Europe/Berlin]")"#
    );
    // Without a zone a day is 24 hours at the value's own offset.
    assert_eq!(
        run(&format!("g.inject({start}).date_add(DT.day, 1).next()")),
        r#"datetime("2023-03-26T12:00:00+01:00")"#
    );
    // Fixed units move the instant; the zone gives the offset at the result.
    assert_eq!(
        run(&format!(
            r#"g.inject({start}).dateAdd(DT.hour, 24, "Europe/Berlin").next()"#
        )),
        r#"datetime("2023-03-26T13:00:00+02:00[Europe/Berlin]")"#
    );
    // A duration, in the zone too.
    assert_eq!(
        run(&format!(
            r#"g.inject({start}).date_add(duration("P1DT1H")).next()"#
        )),
        r#"datetime("2023-03-26T13:00:00+01:00")"#
    );
    assert_eq!(
        run(&format!(
            r#"g.inject({start}).dateAdd(duration("P1D"), "Europe/Berlin").next()"#
        )),
        r#"datetime("2023-03-26T12:00:00+02:00[Europe/Berlin]")"#
    );
    // An unknown zone fails before anything runs.
    let err = fails(r#"g.inject().date_add(DT.day, 1, "Mars/Olympus").to_list()"#);
    assert!(err.contains("unknown time zone"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// date_diff / age
// ---------------------------------------------------------------------------------------------

#[test]
fn date_diff_is_tinkerpops_in_milliseconds() {
    assert_eq!(
        both(
            r#"g.inject(datetime("2023-08-02T00:00:00Z")).date_diff(datetime("2023-08-09T00:00:00Z")).next()"#,
            r#"g.inject(datetime("2023-08-02T00:00:00Z")).dateDiff(datetime("2023-08-09T00:00:00Z")).next()"#,
        ),
        "-604800000"
    );
    // A traversal argument (its first result) and null (the epoch).
    assert_eq!(
        run(
            r#"g.inject(datetime("2023-08-08T00:00:00Z")).date_diff(__.constant(datetime("2023-08-01T00:00:00Z"))).next()"#
        ),
        "604800000"
    );
    assert_eq!(
        run(r#"g.inject(datetime("1970-01-01T00:00:01Z")).date_diff(()).next()"#),
        "1000"
    );
    assert_eq!(
        run(r#"g.inject(datetime("1970-01-01T00:00:01Z")).date_diff(__.constant(())).next()"#),
        "1000"
    );
    // Offsets do not matter: the instants are compared.
    assert_eq!(
        run(
            r#"g.inject(datetime("2023-08-02T02:00:00+02:00")).date_diff(datetime("2023-08-02T00:00:00Z")).next()"#
        ),
        "0"
    );
    // Other types of the same kind.
    assert_eq!(
        run(r#"g.inject(date("2023-08-03")).date_diff(date("2023-08-02")).next()"#),
        "86400000"
    );
    assert_eq!(
        run(r#"g.inject(time("10:00")).date_diff(time("09:59:59.5")).next()"#),
        "500"
    );
    let err =
        fails(r#"g.inject(datetime("2023-08-02T00:00:00Z")).date_diff(date("2023-08-02")).next()"#);
    assert!(err.contains("difference"), "{err}");
    let err = fails(
        r#"g.inject(datetime("2023-08-02T00:00:00Z")).date_diff(__.constant(1).limit(0)).next()"#,
    );
    assert!(err.contains("no result"), "{err}");
}

#[test]
fn date_diff_in_units_counts_whole_units_toward_zero() {
    let diff =
        |a: &str, b: &str, unit: &str| run(&format!("g.inject({a}).date_diff({b}, {unit}).next()"));
    assert_eq!(
        diff(r#"date("2023-02-28")"#, r#"date("2023-01-31")"#, "DT.month"),
        "0"
    );
    assert_eq!(
        diff(r#"date("2023-03-31")"#, r#"date("2023-01-31")"#, "DT.month"),
        "2"
    );
    assert_eq!(
        diff(r#"date("2023-01-31")"#, r#"date("2023-03-31")"#, "DT.month"),
        "-2"
    );
    assert_eq!(
        diff(r#"date("2023-08-09")"#, r#"date("2023-08-02")"#, "DT.week"),
        "1"
    );
    assert_eq!(
        diff(
            r#"datetime("2023-08-03T09:00:00Z")"#,
            r#"datetime("2023-08-02T10:00:00Z")"#,
            "DT.hour"
        ),
        "23"
    );
    // In a zone: the day of the spring-forward change is one calendar day of 23 hours.
    assert_eq!(
        run(
            r#"g.inject(datetime("2023-03-26T12:00:00+02:00")).dateDiff(datetime("2023-03-25T12:00:00+01:00"), DT.day, "Europe/Berlin").next()"#
        ),
        "1"
    );
    assert_eq!(
        run(
            r#"g.inject(datetime("2023-03-26T12:00:00+02:00")).date_diff(datetime("2023-03-25T12:00:00+01:00"), DT.hour).next()"#
        ),
        "23"
    );
}

#[test]
fn age_is_postgresqls() {
    assert_eq!(
        run(r#"g.inject(date("2001-04-10")).age(date("1957-06-13")).next()"#),
        r#"duration("P43Y9M27D")"#
    );
    assert_eq!(
        run(r#"g.inject(date("1957-06-13")).age(__.constant(date("2001-04-10"))).next()"#),
        r#"duration("-P43Y9M27D")"#
    );
    assert_eq!(
        run(
            r#"g.inject(datetime("2024-03-15T10:00:00Z")).age(datetime("2023-01-20T12:00:00Z")).next()"#
        ),
        r#"duration("P1Y1M25DT22H")"#
    );
    assert_eq!(
        run(
            r#"g.inject(datetime("2023-03-26T12:00:00+02:00")).age(datetime("2023-03-25T12:00:00+01:00"), "Europe/Berlin").next()"#
        ),
        r#"duration("P1D")"#
    );
    let err = fails(r#"g.inject(date("2001-04-10")).age(()).next()"#);
    assert!(err.contains("null"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// date_trunc / date_part / at_zone / as_local
// ---------------------------------------------------------------------------------------------

#[test]
fn date_trunc_and_date_part() {
    assert_eq!(
        both(
            r#"g.inject(datetime("2023-08-17T09:30:15.5+02:00")).date_trunc(DT.week).next()"#,
            r#"g.inject(datetime("2023-08-17T09:30:15.5+02:00")).dateTrunc(DT.week).next()"#,
        ),
        r#"datetime("2023-08-14T00:00:00+02:00")"#
    );
    assert_eq!(
        run(
            r#"g.inject(datetime("2023-08-17T01:30:00Z")).date_trunc(DT.day, "America/New_York").next()"#
        ),
        r#"datetime("2023-08-16T00:00:00-04:00[America/New_York]")"#
    );
    assert_eq!(
        run(r#"g.inject(duration("P1Y5M3DT4H")).date_trunc(DT.month).next()"#),
        r#"duration("P1Y5M")"#
    );
    let parts = run(
        r#"let d = datetime("2023-08-17T09:30:15.25+02:00"); [DT.year, DT.month, DT.day, DT.hour, DT.second, DT.millisecond, DT.day_of_week, DT.iso_day_of_week, DT.day_of_year, DT.week, DT.quarter, DT.offset].map(|p| g.inject(d).date_part(p).next())"#,
    );
    assert_eq!(parts, "[2023, 8, 17, 9, 15, 250, 4, 4, 229, 33, 3, 7200]");
    assert_eq!(
        run(r#"g.inject(datetime("1970-01-01T00:00:01.5Z")).datePart(DT.epoch).next()"#),
        "1.5"
    );
    assert_eq!(
        run(r#"g.inject(datetime("2023-08-17T09:30:00+02:00")).date_part(DT.hour, "UTC").next()"#),
        "7"
    );
    let err = fails(r#"g.inject(time("10:00")).date_part(DT.year).next()"#);
    assert!(err.contains("not defined for a time"), "{err}");
}

#[test]
fn at_zone_and_as_local() {
    assert_eq!(
        both(
            r#"g.inject(datetime("2023-08-02T12:00:00Z")).at_zone("Europe/Berlin").next()"#,
            r#"g.inject(datetime("2023-08-02T12:00:00Z")).atZone("Europe/Berlin").next()"#,
        ),
        r#"datetime("2023-08-02T14:00:00+02:00[Europe/Berlin]")"#
    );
    // A wall clock in the zone: a skipped time moves forward, a repeated one takes the earlier
    // offset.
    assert_eq!(
        run(
            r#"g.inject(localdatetime("2023-03-26T02:30"), localdatetime("2023-10-29T02:30")).at_zone("Europe/Berlin").to_list()"#
        ),
        r#"[datetime("2023-03-26T03:30:00+02:00[Europe/Berlin]"), datetime("2023-10-29T02:30:00+02:00[Europe/Berlin]")]"#
    );
    assert_eq!(
        run(r#"g.inject(datetime("2023-08-02T12:00:00Z")).at_zone("-07:00").next()"#),
        r#"datetime("2023-08-02T05:00:00-07:00")"#
    );
    assert_eq!(
        both(
            r#"g.inject(datetime("2023-08-02T12:00:00+02:00")).as_local().next()"#,
            r#"g.inject(datetime("2023-08-02T12:00:00+02:00")).asLocal().next()"#,
        ),
        r#"localdatetime("2023-08-02T12:00:00")"#
    );
    assert_eq!(
        run(r#"g.inject(datetime("2023-08-02T12:00:00Z")).as_local("Asia/Tokyo").next()"#),
        r#"localdatetime("2023-08-02T21:00:00")"#
    );
    let err = fails(r#"g.inject(date("2023-08-02")).at_zone("UTC").next()"#);
    assert!(err.contains("at_zone"), "{err}");
    let err = fails(r#"g.inject(date("2023-08-02")).as_local().next()"#);
    assert!(err.contains("as_local"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// Values in traversals: predicates, ordering, typeOf, properties
// ---------------------------------------------------------------------------------------------

fn events() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::empty());
    run_on(
        graph.clone(),
        r#"g.add_v("event").property("name", "a").property("at", datetime("2023-08-08T00:00:00Z"))
            .add_v("event").property("name", "b").property("at", datetime("2023-08-08T02:00:00+02:00"))
            .add_v("event").property("name", "c").property("at", datetime("2020-01-01T00:00:00Z"))
            .iterate()"#,
    );
    graph
}

#[test]
fn dates_compare_order_and_type_check_in_traversals() {
    let graph = events();
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().has("at", P.gt(datetime("2021-01-01T00:00:00Z"))).values("name").order().to_list()"#
        ),
        r#"["a", "b"]"#
    );
    // `b` is the same instant as `a`.
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().has("at", datetime("2023-08-08T00:00:00Z")).values("name").order().to_list()"#
        ),
        r#"["a", "b"]"#
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().values("at").dedup().count().next()"#
        ),
        "2"
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().order().by("at").values("name").to_list()"#
        )
        .split(", ")
        .next()
        .unwrap(),
        r#"["c""#
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().values("at").is(P.typeOf(GType.DATETIME)).count().next()"#
        ),
        "3"
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().values("at").is(P.typeOf("OffsetDateTime")).count().next()"#
        ),
        "3"
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().values("name").is(P.typeOf(GType.DATETIME)).count().next()"#
        ),
        "0"
    );
    assert_eq!(
        run(
            r#"g.inject(date("2023-08-02"), duration("P1D"), time("10:00"), localdatetime("2023-08-02T10:00")).is(P.typeOf(GType.DATE).or(P.typeOf(GType.TIME))).to_list()"#
        ),
        r#"[date("2023-08-02"), time("10:00:00")]"#
    );
    // A value read by a terminal feeds back into a filter as a date.
    assert_eq!(
        run_on(
            graph,
            r#"let first = g.v().has("name", "c").values("at").next(); g.v().has("at", first).values("name").to_list()"#
        ),
        r#"["c"]"#
    );
}

#[test]
fn script_values_have_types_getters_and_operators() {
    assert_eq!(
        run(
            r#"[type_of(datetime("2023-08-02T00:00:00Z")), type_of(date("2023-08-02")), type_of(localdatetime("2023-08-02T00:00")), type_of(time("10:00")), type_of(duration("P1D"))]"#
        ),
        r#"["datetime", "date", "localdatetime", "time", "duration"]"#
    );
    assert_eq!(
        run(
            r#"let d = datetime("2023-08-02T09:30:15.25+02:00"); [d.year, d.month, d.day, d.hour, d.minute, d.second, d.microsecond, d.offset, d.epoch_millis, d.epochMillis]"#
        ),
        "[2023, 8, 2, 9, 30, 15, 250000, 7200, 1690961415250, 1690961415250]"
    );
    assert_eq!(
        run(r#"let p = duration("P1Y2M3DT4H"); [p.months, p.days, p.micros]"#),
        "[14, 3, 14400000000]"
    );
    // Comparisons within a type (a datetime by its instant).
    assert_eq!(
        run(
            r#"[datetime("2023-08-02T12:00:00Z") == datetime("2023-08-02T14:00:00+02:00"), date("2023-08-02") < date("2023-08-03"), duration("P1M") == duration("P30D"), duration("P1M") > duration("P29D"), date("2023-08-02") == "2023-08-02"]"#
        ),
        "[true, true, false, true, false]"
    );
    // Arithmetic with durations; the difference of two values of one type.
    assert_eq!(
        run(
            r#"[date("2024-01-31") + duration("P1M"), datetime("2023-08-02T00:00:00Z") - duration("PT1H"), date("2023-08-12") - date("2023-08-02"), datetime("2023-08-03T01:00:00Z") - datetime("2023-08-02T00:00:00Z"), time("10:00") - time("08:30"), -duration("P1D"), duration("P1D") + duration("PT1H")]"#
        ),
        r#"[date("2024-02-29"), datetime("2023-08-01T23:00:00Z"), duration("P10D"), duration("P1DT1H"), duration("PT1H30M"), duration("-P1D"), duration("P1DT1H")]"#
    );
    // Printing is the DSL literal; the as_string() step gives the canonical text.
    assert_eq!(
        run(
            r#"[datetime("2023-08-02T00:00Z").to_string(), `${date("2023-08-02")}`, g.inject(time("10:00"), duration("P1D")).as_string().to_list()]"#
        ),
        r#"["datetime(\"2023-08-02T00:00:00Z\")", "date(\"2023-08-02\")", ["10:00:00", "P1D"]]"#
    );
    // Constructors fail with the parse reason; operators of mismatched types name both.
    let err = fails(r#"date("2023-02-30")"#);
    assert!(err.contains("2023-02-30"), "{err}");
    assert!(err.contains("date()"), "{err}");
    let err = fails(r#"datetime("2023-08-02T00:00:00Z") + 1"#);
    assert!(err.contains("datetime"), "{err}");
    assert!(!err.contains("graphersal::"), "{err}");
}

#[test]
fn dates_are_stored_and_read_back_through_the_dsl() {
    let graph = events();
    let value = graph
        .read()
        .traversal()
        .v(None)
        .has("name", "b")
        .values("at")
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(temporal_text(&value), "2023-08-08T02:00:00+02:00");
    assert_eq!(
        run_on(
            graph,
            r#"g.v().has("name", "b").values("at").date_add(DT.day, 1).next()"#
        ),
        r#"datetime("2023-08-09T02:00:00+02:00")"#
    );
}

// ---------------------------------------------------------------------------------------------
// now() / today()
// ---------------------------------------------------------------------------------------------

#[test]
fn now_is_one_value_per_evaluation() {
    assert_eq!(
        run("let a = now(); let b = now(); [a == b, a == now(), type_of(a)]"),
        r#"[true, true, "datetime"]"#
    );
    // Inside a traversal it is the same value too.
    assert_eq!(
        run("let a = now(); g.inject(now(), now()).is(a).count().next()"),
        "2"
    );
    assert_eq!(run("type_of(today())"), r#""date""#);
}

#[test]
fn now_can_be_pinned_by_the_host() {
    let graph: Arc<Graph> = Arc::new(GraphSource::empty());
    let mut engine = crate::script_host::engine_with_options(&Default::default(), &[]);
    graphersal::script::pin_now(&mut engine, Some(dt("2023-08-02T23:30:00-07:00")));
    let eval = |engine: &graphersal::script::rhai::Engine, script: &str| -> String {
        let mut scope = crate::script_host::graph_scope_with_options(graph.clone(), &[]);
        let value: Dynamic = engine
            .eval_with_scope(&mut scope, script)
            .unwrap_or_else(|err| panic!("{script}: {err}"));
        debug_value(&value)
    };
    assert_eq!(
        eval(&engine, "now()"),
        r#"datetime("2023-08-02T23:30:00-07:00")"#
    );
    // today() is the UTC date of now(); the date in a zone through as_local().
    assert_eq!(eval(&engine, "today()"), r#"date("2023-08-03")"#);
    assert_eq!(
        eval(
            &engine,
            r#"g.inject(now()).as_local("America/Los_Angeles").as_date(GType.DATE).next()"#
        ),
        r#"date("2023-08-02")"#
    );
    assert_eq!(
        eval(
            &engine,
            r#"g.inject(1).constant(now()).date_add(DT.day, 1).next()"#
        ),
        r#"datetime("2023-08-03T23:30:00-07:00")"#
    );
    // today(zone): the date of now() in that zone (an IANA name or an offset).
    assert_eq!(
        eval(&engine, r#"today("America/Los_Angeles")"#),
        r#"date("2023-08-02")"#
    );
    assert_eq!(eval(&engine, r#"today("+02:00")"#), r#"date("2023-08-03")"#);
    assert_eq!(eval(&engine, r#"today("UTC") == today()"#), "true");
    // Unpinned again: the system clock.
    graphersal::script::pin_now(&mut engine, None);
    assert_ne!(
        eval(&engine, "now()"),
        r#"datetime("2023-08-02T23:30:00-07:00")"#
    );
}

#[test]
fn a_saved_query_sees_the_callers_now() {
    let graph: Arc<Graph> = Arc::new(GraphSource::empty());
    run_on(
        graph.clone(),
        r#"g.define_query(#{name: "stamp", body: "fn stamp() { g.inject(now()) }"})"#,
    );
    assert_eq!(
        run_on(graph, r#"let a = now(); g.query("stamp").next() == a"#),
        "true"
    );
}

// ---------------------------------------------------------------------------------------------
// Rust API, profile names, spelling
// ---------------------------------------------------------------------------------------------

#[test]
fn the_rust_api_builds_the_same_steps() {
    let graph = GraphSource::empty();
    let lock = graph.read();
    let start = ElementProperty::from(dt("2023-03-25T12:00:00+01:00"));
    let one = |traversal: GraphTraversalSource<'_, &TraversalGraph>| -> String {
        temporal_text(&traversal.next().unwrap().unwrap())
    };
    assert_eq!(
        one(lock
            .traversal()
            .inject(vec![start.clone()])
            .date_add(DT::Day, 1)),
        "2023-03-26T12:00:00+01:00"
    );
    assert_eq!(
        one(lock
            .traversal()
            .inject(vec![start.clone()])
            .date_add_in(DT::Day, 1, "Europe/Berlin")),
        "2023-03-26T12:00:00+02:00[Europe/Berlin]"
    );
    let p1d: Duration = "P1D".parse().unwrap();
    assert_eq!(
        one(lock
            .traversal()
            .inject(vec![start.clone()])
            .date_add_duration(p1d)),
        "2023-03-26T12:00:00+01:00"
    );
    assert_eq!(
        one(lock
            .traversal()
            .inject(vec!["2023-08-02"])
            .as_date_to(GType::DATE)),
        "2023-08-02"
    );
    assert_eq!(
        one(lock.traversal().inject(vec!["P2W"]).as_duration()),
        "P14D"
    );
    assert_eq!(
        one(lock
            .traversal()
            .inject(vec![start.clone()])
            .date_trunc(DT::Month)),
        "2023-03-01T00:00:00+01:00"
    );
    assert_eq!(
        one(lock.traversal().inject(vec![start.clone()]).at_zone("UTC")),
        "2023-03-25T11:00:00Z[UTC]"
    );
    assert_eq!(
        one(lock.traversal().inject(vec![start.clone()]).as_local()),
        "2023-03-25T12:00:00"
    );
    let earlier = ElementProperty::from(dt("2023-03-25T11:00:00+01:00"));
    let millis = lock
        .traversal()
        .inject(vec![start.clone()])
        .date_diff(earlier.clone())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(millis, ElementProperty::from(3_600_000i64));
    let hours = lock
        .traversal()
        .inject(vec![start.clone()])
        .date_diff_traversal_in(__::constant(earlier.clone()), DT::Minute, None)
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(hours, ElementProperty::from(60i64));
    assert_eq!(
        one(lock
            .traversal()
            .inject(vec![start.clone()])
            .age(earlier, None)),
        "PT1H"
    );
    let part = lock
        .traversal()
        .inject(vec![start])
        .date_part(DT::DayOfWeek)
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(part, ElementProperty::from(6i64));
    // A temporal value is a property value.
    let value = ElementProperty::from(Temporal::from(p1d));
    assert_eq!(temporal_text(&value), "P1D");
}

#[test]
fn profile_shows_the_canonical_step_names_in_both_spellings() {
    let profile = run(
        r#"g.inject(datetime("2023-08-02T00:00:00Z")).as_date().date_add(DT.day, 1, "Europe/Berlin").date_diff(__.constant(datetime("2023-08-01T00:00:00Z")), DT.day).profile().to_table()"#,
    );
    for name in [
        "as_date()",
        r#"date_add(DT.day, 1, \"Europe/Berlin\")"#,
        r#"date_diff(__.constant(datetime(\"2023-08-01T00:00:00Z\")), DT.day)"#,
    ] {
        assert!(profile.contains(name), "{name} missing in {profile}");
    }
    let camel = run(
        r#"g.with("render.spelling", "camel").inject("2023-08-02").as_date(GType.DATE).date_trunc(DT.month).profile().to_table()"#,
    );
    for name in ["asDate(GType.DATE)", "dateTrunc(DT.month)"] {
        assert!(camel.contains(name), "{name} missing in {camel}");
    }
}

#[test]
fn errors_are_respelled_in_camel_case() {
    let err =
        fails(r#"g.with("render.spelling", "camel").inject(5).date_add(DT.day, 1).to_list()"#);
    assert!(err.contains("dateAdd(DT.day, 1)"), "{err}");
    assert!(!err.contains("date_add(DT.day"), "{err}");
}

#[test]
fn date_steps_ask_no_permission() {
    use graphersal::auth::AccessPolicy;
    let graph: Arc<Graph> = Arc::new(GraphSource::empty());
    let value = graphersal::script::eval_value_with_limits(
        graph,
        r#"g.inject("2023-08-02").as_date().date_add(DT.day, 1).date_trunc(DT.month).as_local().to_list()"#,
        Default::default(),
        &Default::default(),
        Arc::new(AccessPolicy::deny_all()),
    )
    .unwrap();
    assert_eq!(
        debug_value(&value),
        r#"[localdatetime("2023-08-01T00:00:00")]"#
    );
}

#[test]
fn today_refuses_an_unknown_zone() {
    let err = fails(r#"today("Mars/Olympus")"#);
    assert!(err.contains("Mars/Olympus"), "{err}");
}

#[test]
fn script_values_give_their_canonical_text() {
    assert_eq!(
        both(
            r#"datetime("2023-08-02T09:30+02:00").as_string()"#,
            r#"datetime("2023-08-02T09:30+02:00").asString()"#
        ),
        r#""2023-08-02T09:30:00+02:00""#
    );
    assert_eq!(run(r#"date("2024-02-29").as_string()"#), r#""2024-02-29""#);
    assert_eq!(
        run(r#"localdatetime("2023-08-02T09:30:00.25").as_string()"#),
        r#""2023-08-02T09:30:00.250""#
    );
    assert_eq!(run(r#"time("09:30").as_string()"#), r#""09:30:00""#);
    assert_eq!(
        run(r#"duration("P1Y2M3DT4H").as_string()"#),
        r#""P1Y2M3DT4H""#
    );
    // The same text as the as_string() step.
    assert_eq!(
        run(r#"g.inject(date("2024-02-29")).as_string().next() == date("2024-02-29").as_string()"#),
        "true"
    );
}

/// The GQL text forms (3g: Neo4j's `toString`, the extended ISO parser) leave the script's
/// text untouched: zero seconds and trailing fraction zeros are still written, a negative
/// duration keeps one leading sign, and the script constructors read only their own forms.
#[test]
fn the_gql_text_forms_leave_the_script_text_unchanged() {
    assert_eq!(run(r#"time("12:00")"#), r#"time("12:00:00")"#);
    assert_eq!(
        run(r#"localdatetime("1984-10-11T12:31")"#),
        r#"localdatetime("1984-10-11T12:31:00")"#
    );
    assert_eq!(
        run(r#"duration("PT23H59M59.9S")"#),
        r#"duration("PT23H59M59.900S")"#
    );
    assert_eq!(
        run(r#"duration("PT-1M-0.001S").as_string()"#),
        r#""-PT1M0.001S""#
    );
    assert_eq!(
        run(r#"g.inject(time("12:00")).as_string().next()"#),
        r#""12:00:00""#
    );
    for gql_only in [
        r#"date("2015W302")"#,
        r#"time("2140")"#,
        r#"duration("P0.75M")"#,
    ] {
        let err = fails(gql_only);
        assert!(err.contains("cannot parse"), "{gql_only}: {err}");
    }
}
