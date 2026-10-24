//! The diagnostics of the date steps and the script's date and time values: every error kind
//! names the step, and its `help()` names the step or function that fixes it with a runnable
//! example (rule 6). Failures through the Rust API and through the DSL.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::error::{TraverserError, ValueError, format_diagnostic};
use graphersal::prelude::*;
use graphersal::temporal::DT;

use crate::script_host::eval_value;

/// The innermost value error of a failed traversal.
fn value_error(err: &TraverserError) -> &ValueError {
    match err {
        TraverserError::Value(value) => value,
        TraverserError::StepFailed { source, .. } => value_error(source),
        other => panic!("not a value error: {other:?}"),
    }
}

/// The rendered diagnostic (message and help) of a failing script.
fn script_failure(script: &str) -> String {
    match eval_value(Arc::new(GraphSource::empty()), script) {
        Ok(_) => panic!("{script} must fail"),
        Err(err) => err.to_string(),
    }
}

/// The diagnostic and the help of a failing Rust traversal.
fn rust_failure(
    build: impl FnOnce(
        GraphTraversalSource<'_, &TraversalGraph>,
    ) -> Result<Vec<ElementProperty>, Box<TraverserError>>,
) -> (String, String) {
    let graph = GraphSource::empty();
    let lock = graph.read();
    let err = build(lock.traversal()).expect_err("must fail");
    let text = format_diagnostic(&*err);
    let help = text.split("Help:").nth(1).unwrap_or_default().to_string();
    (text, help)
}

#[test]
fn a_value_that_is_not_a_date_names_the_conversion() {
    let (text, help) =
        rust_failure(|g| g.inject(vec!["2023-08-02"]).date_add(DT::Day, 1).to_list());
    assert!(
        text.contains("The date_add() step takes a date or time as its incoming value, but got string \"2023-08-02\""),
        "{text}"
    );
    assert!(
        help.contains("g.inject(\"2023-08-02T00:00:00Z\").as_date().date_add(DT.day, 1)"),
        "{help}"
    );
    let (_, help) = rust_failure(|g| {
        g.inject(vec![1690934400000i64])
            .date_part(DT::Year)
            .to_list()
    });
    assert!(
        help.contains("g.inject(1690934400000).as_date().date_part(DT.year)"),
        "{help}"
    );
    let text = script_failure(r#"g.add_v("x").date_trunc(DT.day).to_list()"#);
    assert!(text.contains("a vertex"), "{text}");
    assert!(text.contains("is(P.typeOf(GType.DATETIME))"), "{text}");
}

#[test]
fn a_traversal_argument_without_a_result_names_the_fix() {
    let text = script_failure(
        r#"g.inject(datetime("2023-08-08T00:00:00Z")).date_diff(__.constant(1).limit(0)).to_list()"#,
    );
    assert!(text.contains("traversal argument"), "{text}");
    assert!(text.contains("no result"), "{text}");
    assert!(
        text.contains(
            "g.inject(datetime(\"2023-08-08T00:00:00Z\")).date_diff(__.constant(datetime(\"2023-08-01T00:00:00Z\")))"
        ),
        "{text}"
    );
    let text = script_failure(
        r#"g.inject(datetime("2023-08-08T00:00:00Z")).date_diff(__.constant("2023-08-01")).to_list()"#,
    );
    assert!(text.contains("string \"2023-08-01\""), "{text}");
}

#[test]
fn as_date_failures_start_like_tinkerpops() {
    let graph = GraphSource::empty();
    let lock = graph.read();
    let err = lock
        .traversal()
        .inject(vec![1.5f64])
        .as_date()
        .to_list()
        .unwrap_err();
    assert!(matches!(
        value_error(&err),
        ValueError::TemporalParse {
            step: "as_date",
            target: "datetime",
            ..
        }
    ));
    let text = format_diagnostic(&*err);
    assert!(
        text.contains("Can't parse float64 1.5 as a datetime"),
        "{text}"
    );
    assert!(
        text.contains("as_number(GType.LONG) truncates a float first"),
        "{text}"
    );
    let text = script_failure(r#"g.inject("tomorrow").as_date().to_list()"#);
    assert!(
        text.contains("Can't parse string \"tomorrow\" as a datetime"),
        "{text}"
    );
    assert!(text.contains("g.inject(1690934400000).as_date()"), "{text}");
    let text = script_failure(r#"g.inject("2023-08-02T00:00:00Z").as_date(GType.DATE).to_list()"#);
    assert!(text.contains("as a date"), "{text}");
    assert!(
        text.contains("as_date(GType.DATE) reads a date text"),
        "{text}"
    );
    let text = script_failure(r#"g.inject("1 day").as_duration().to_list()"#);
    assert!(
        text.contains("Can't parse string \"1 day\" as a duration"),
        "{text}"
    );
    assert!(text.contains("duration(\"P1D\")"), "{text}");
    let text = script_failure(r#"g.inject().as_date(GType.STRING).to_list()"#);
    assert!(text.contains("GType.STRING"), "{text}");
}

#[test]
fn arithmetic_failures_name_the_rule() {
    let text = script_failure(
        r#"g.inject(datetime("9999-12-31T00:00:00Z")).date_add(DT.year, 1).to_list()"#,
    );
    assert!(text.contains("The date_add() step failed"), "{text}");
    assert!(
        text.contains("date_add(DT.year, 10000) goes too far"),
        "{text}"
    );
    let text = script_failure(r#"g.inject(date("2023-08-02")).date_add(DT.hour, 1).to_list()"#);
    assert!(
        text.contains("adding the unit hour is not defined for a date"),
        "{text}"
    );
    assert!(text.contains("as_date(GType.LOCALDATETIME)"), "{text}");
    let text = script_failure(
        r#"g.inject(datetime("2023-08-02T00:00:00Z")).date_part(DT.day_of_week, "Europe/Berlin").date_trunc(DT.epoch).to_list()"#,
    );
    assert!(text.contains("date_trunc"), "{text}");
    let text = script_failure(
        r#"g.inject(date("2023-08-02")).date_diff(datetime("2023-08-02T00:00:00Z")).to_list()"#,
    );
    assert!(text.contains("same type"), "{text}");
}

#[test]
fn an_unknown_zone_fails_before_execution_and_names_the_forms() {
    let graph = GraphSource::empty();
    let lock = graph.read();
    // No traverser reaches the step: the plan-time check still fails.
    let err = lock
        .traversal()
        .inject(Vec::<i64>::new())
        .at_zone("Europe/Atlantis")
        .to_list()
        .unwrap_err();
    assert!(matches!(
        value_error(&err),
        ValueError::Temporal {
            step: "at_zone",
            ..
        }
    ));
    let text = format_diagnostic(&*err);
    assert!(
        text.contains("unknown time zone \"Europe/Atlantis\""),
        "{text}"
    );
    assert!(text.contains("at_zone(\"Europe/Berlin\")"), "{text}");
    assert!(text.contains("\"+02:00\""), "{text}");
}

#[test]
fn a_constructor_names_the_parse_reason_and_the_forms() {
    let text = script_failure(r#"datetime("2023-08-02T25:00:00Z")"#);
    assert!(
        text.contains("The datetime() step failed: cannot parse"),
        "{text}"
    );
    assert!(text.contains("2023-08-02T25:00:00Z"), "{text}");
    assert!(text.contains("date(\"2023-08-02\")"), "{text}");
    let text = script_failure(r#"duration("one day")"#);
    assert!(text.contains("P"), "{text}");
    assert!(text.contains("duration(\"P1DT2H\")"), "{text}");
}

#[test]
fn diagnostics_name_temporal_types_by_their_value_type_names() {
    let text = script_failure(r#"g.inject(1).limit(datetime("2023-08-02T00:00:00Z"))"#);
    assert!(text.contains("datetime"), "{text}");
    assert!(!text.contains("graphersal::"), "{text}");
    let text = script_failure(r#"duration("P1D") * 2"#);
    assert!(text.contains("duration"), "{text}");
    assert!(!text.contains("graphersal::"), "{text}");
}

#[test]
fn a_saved_query_date_argument_without_an_offset_names_the_constructor() {
    let text = script_failure(
        r#"g.define_query(#{name: "after", params: #{t: #{schema: #{type: "string", format: "date-time"}}}, body: "fn after(t) { g.inject(t) }"});
           g.query("after", #{t: "2023-08-02T09:30"}).to_list()"#,
    );
    assert!(text.contains("UTC offset or time zone"), "{text}");
    assert!(
        text.contains(r#"datetime("2024-05-01T09:30:00+02:00")"#),
        "{text}"
    );
}

#[test]
fn offsettime_conversions_and_types_are_named_in_the_help() {
    // Temporal v2: the help names the new type and its as_date() target (rule 6).
    let text = script_failure(r#"g.inject("09:30").as_date(GType.OFFSETTIME).next()"#);
    assert!(
        text.contains("as_date(GType.OFFSETTIME) reads a time text with its UTC offset"),
        "{text}"
    );
    assert!(
        text.contains(r#"g.inject(time("09:30")).as_date(GType.OFFSETTIME)"#),
        "{text}"
    );
    let text = script_failure(r#"g.inject(true).date_add(DT.hour, 1).next()"#);
    assert!(text.contains(r#"offsettime("09:30:00+02:00")"#), "{text}");
    let text = script_failure(r#"g.inject(offsettime("10:00Z")).date_add(DT.day, 1).next()"#);
    assert!(
        text.contains("a time and an offsettime have no days or months"),
        "{text}"
    );
}
