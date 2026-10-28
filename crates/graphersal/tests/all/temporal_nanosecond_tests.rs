//! Nanosecond resolution of the temporal types (temporal v2, stage 1): the range edges of the
//! years -9999..9999 at a nanosecond, overflow as an error, truncation to every unit, the parts,
//! text round trips with one to nine fraction digits, ordering, durations, schema coercion, the
//! Rhai and GQL forms, the property index, and a Store that keeps a value with a part below the
//! microsecond (file format version 3; stage 1 refused it under version 2).

use graphersal::prelude::*;
use graphersal::temporal::{
    DT, Date, DateTime, Duration, LocalDateTime, Temporal, TemporalKind, Time,
};

fn ldt(text: &str) -> LocalDateTime {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn dt(text: &str) -> DateTime {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn time(text: &str) -> Time {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn dur(text: &str) -> Duration {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

#[test]
fn the_range_edges_hold_every_nanosecond() {
    let last = ldt("9999-12-31T23:59:59.999999999");
    let first = ldt("-9999-01-01T00:00:00");
    assert_eq!(last.to_string(), "9999-12-31T23:59:59.999999999");
    assert_eq!(first.to_string(), "-9999-01-01T00:00:00");
    // ±3.2e20 nanoseconds: beyond an i64.
    assert!(last.nanos() > i64::MAX as i128);
    assert!(first.nanos() < i64::MIN as i128);
    assert!(LocalDateTime::from_nanos(last.nanos() + 1).is_err());
    assert!(LocalDateTime::from_nanos(first.nanos() - 1).is_err());
    assert_eq!(LocalDateTime::from_nanos(last.nanos()).unwrap(), last);
    assert_eq!(last.micros(), 253_402_300_799_999_999);
    assert_eq!(last.time().nanosecond(), 999_999_999);
    assert_eq!(last.time().microsecond(), 999_999);
    // A datetime at the edge, and one nanosecond past it.
    let edge = dt("9999-12-31T23:59:59.999999999Z");
    assert_eq!(edge.epoch_nanos(), last.nanos());
    assert!(DateTime::from_nanos(edge.epoch_nanos() + 1, 0).is_err());
    assert!(DateTime::from_nanos(edge.epoch_nanos(), 3600).is_err());
    assert!(DateTime::from_nanos(edge.epoch_nanos(), -3600).is_ok());
    assert!(DateTime::from_nanos(i128::MAX, 0).is_err());
    assert!(DateTime::from_nanos(i128::MIN, 0).is_err());
    // A time of day.
    assert_eq!(
        Time::from_nanos_of_day(86_400_000_000_000 - 1)
            .unwrap()
            .to_string(),
        "23:59:59.999999999"
    );
    assert!(Time::from_nanos_of_day(86_400_000_000_000).is_err());
    assert!(Time::from_nanos_of_day(-1).is_err());
    assert!(Time::from_hms_nano(0, 0, 0, 1_000_000_000).is_err());
    // A duration's time part holds an i64 of seconds plus nanoseconds (temporal v2 stage 2b,
    // java.time.Duration's range).
    let max = Duration::with_nanos(0, 0, i64::MAX as i128 * 1_000_000_000 + 999_999_999).unwrap();
    assert_eq!(max.micros(), None);
    assert!(Duration::with_nanos(0, 0, i64::MAX as i128 * 1_000_000_000 + 1_000_000_000).is_err());
    let min = Duration::with_nanos(0, 0, i64::MIN as i128 * 1_000_000_000).unwrap();
    assert!(Duration::with_nanos(0, 0, i64::MIN as i128 * 1_000_000_000 - 1).is_err());
    assert!(min.checked_neg().is_err());
    assert!(max.checked_add(Duration::from_nanos(1).unwrap()).is_err());
    assert_eq!(min.to_string().parse::<Duration>().unwrap(), min);
    assert_eq!(max.to_string().parse::<Duration>().unwrap(), max);
}

#[test]
fn text_round_trips_with_one_to_nine_fraction_digits() {
    for digits in 1..=9usize {
        let fraction: String = "123456789"[..digits].to_string();
        let text = format!("12:34:56.{fraction}");
        let value = time(&text);
        let expected_nanos = fraction.parse::<u32>().unwrap() * 10u32.pow(9 - digits as u32);
        assert_eq!(value.nanosecond(), expected_nanos, "{text}");
        // Printed in groups of three digits, trailing groups of zeros dropped.
        let printed = value.to_string();
        let back = time(&printed);
        assert_eq!(back, value, "{text} -> {printed}");
        let fraction_len = printed.split('.').nth(1).map_or(0, str::len);
        assert_eq!(fraction_len, digits.div_ceil(3) * 3, "{printed}");
        for kind in [
            TemporalKind::LocalDateTime,
            TemporalKind::DateTime,
            TemporalKind::Duration,
        ] {
            let text = match kind {
                TemporalKind::LocalDateTime => format!("2024-02-29T12:34:56.{fraction}"),
                TemporalKind::DateTime => format!("2024-02-29T12:34:56.{fraction}+05:45"),
                _ => format!("-P1DT12H34M56.{fraction}S"),
            };
            let value = Temporal::parse_as(kind, &text).unwrap();
            let back = Temporal::parse_as(kind, &value.to_string()).unwrap();
            assert_eq!(back, value, "{text}");
            assert_eq!(back.to_string(), value.to_string(), "{text}");
        }
    }
    // Values without a part below the microsecond print as before.
    assert_eq!(time("10:00:00.250").to_string(), "10:00:00.250");
    assert_eq!(time("10:00:00.250500").to_string(), "10:00:00.250500");
    assert_eq!(time("10:00:00.250500001").to_string(), "10:00:00.250500001");
    assert_eq!(dur("PT0.000000001S").to_string(), "PT0.000000001S");
    assert_eq!(dur("-PT0.000000001S").to_string(), "-PT0.000000001S");
    assert_eq!(dur("P1MT-0.000000001S").to_string(), "P1MT-0.000000001S");
    // Ten digits are refused, never cut.
    assert!("12:00:00.1234567891".parse::<Time>().is_err());
}

#[test]
fn values_one_nanosecond_apart_are_different_and_ordered() {
    let a = ldt("2024-01-01T00:00:00.000000001");
    let b = ldt("2024-01-01T00:00:00.000000002");
    assert!(a < b);
    assert_ne!(a, b);
    let x = dt("2024-01-01T00:00:00.000000001Z");
    let y = dt("2024-01-01T02:00:00.000000001+02:00");
    assert_eq!(x, y);
    assert!(x < dt("2024-01-01T00:00:00.000000002Z"));
    // Before the epoch: rounding down keeps the order.
    let before = ldt("1969-12-31T23:59:59.999999999");
    assert!(before < ldt("1970-01-01T00:00:00"));
    assert_eq!(before.micros(), -1);
    assert_eq!(before.nanos(), -1);
    assert!(dur("PT0.000000001S") < dur("PT0.000000002S"));
    assert!(time("00:00:00.000000001") > time("00:00:00"));
    // The sub-microsecond flag.
    assert!(Temporal::LocalDateTime(a).has_sub_micros());
    assert!(!Temporal::LocalDateTime(ldt("2024-01-01T00:00:00.000001")).has_sub_micros());
    assert!(!Temporal::Date(Date::from_ymd(2024, 1, 1).unwrap()).has_sub_micros());
}

#[test]
fn arithmetic_with_nanoseconds_and_overflow_is_an_error() {
    // The steps are tested through the script below; here the values themselves.
    let max = ldt("9999-12-31T23:59:59.999999999");
    assert!(
        Temporal::LocalDateTime(max)
            .to_string()
            .ends_with(".999999999")
    );
    let d = dur("PT0.000000001S");
    assert_eq!(d.nanos(), 1);
    assert_eq!(d.micros(), Some(0));
    let sum = d.checked_add(dur("PT0.999999999S")).unwrap();
    assert_eq!(sum.to_string(), "PT1S");
    assert_eq!(dur("-PT0.0000015S").micros(), Some(-1));
}

#[cfg(feature = "script")]
mod script {
    use std::sync::Arc;

    use graphersal::prelude::*;

    use crate::script_host::{debug_value, eval_value};

    fn run(script: &str) -> String {
        match eval_value(Arc::new(GraphSource::empty()), script) {
            Ok(value) => debug_value(&value),
            Err(err) => panic!("{script}: {err}"),
        }
    }

    fn fails(script: &str) -> String {
        match eval_value(Arc::new(GraphSource::empty()), script) {
            Ok(value) => panic!("{script} must fail, got {}", debug_value(&value)),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn the_rhai_values_keep_nanoseconds() {
        assert_eq!(run(r#"time("12:00:00.123456789").nanosecond"#), "123456789");
        assert_eq!(run(r#"time("12:00:00.123456789").microsecond"#), "123456");
        assert_eq!(
            run(r#"localdatetime("2024-01-01T00:00:00.000000007").nanosecond"#),
            "7"
        );
        assert_eq!(
            run(r#"datetime("2024-01-01T00:00:00.000000007Z").nanosecond"#),
            "7"
        );
        assert_eq!(run(r#"duration("PT1.000000005S").nanos"#), "1000000005");
        assert_eq!(run(r#"duration("PT1.000000005S").micros"#), "1000000");
        assert!(fails(r#"duration("PT2562047788H").nanos"#).contains("range"));
        assert_eq!(
            run(r#"time("12:00:00.000000001")"#),
            r#"time("12:00:00.000000001")"#
        );
        // `-` of two values.
        assert_eq!(
            run(
                r#"localdatetime("2024-01-02T00:00:00.000000001") - localdatetime("2024-01-01T00:00:00")"#
            ),
            r#"duration("P1DT0.000000001S")"#
        );
        assert_eq!(
            run(r#"time("00:00:00.000000003") - time("00:00:00.000000001")"#),
            r#"duration("PT0.000000002S")"#
        );
        assert_eq!(run("DT.nanosecond"), "DT.nanosecond");
    }

    #[test]
    fn date_steps_take_the_nanosecond_unit() {
        let value = r#"localdatetime("2024-05-17T13:45:56.123456789")"#;
        for (unit, expected) in [
            ("nanosecond", "2024-05-17T13:45:56.123456789"),
            ("microsecond", "2024-05-17T13:45:56.123456"),
            ("millisecond", "2024-05-17T13:45:56.123"),
            ("second", "2024-05-17T13:45:56"),
            ("minute", "2024-05-17T13:45:00"),
            ("hour", "2024-05-17T13:00:00"),
            ("day", "2024-05-17T00:00:00"),
            ("week", "2024-05-13T00:00:00"),
            ("month", "2024-05-01T00:00:00"),
            ("quarter", "2024-04-01T00:00:00"),
            ("year", "2024-01-01T00:00:00"),
            ("decade", "2020-01-01T00:00:00"),
            ("century", "2001-01-01T00:00:00"),
            ("millennium", "2001-01-01T00:00:00"),
        ] {
            assert_eq!(
                run(&format!("g.inject({value}).date_trunc(DT.{unit}).next()")),
                format!("localdatetime(\"{expected}\")"),
                "{unit}"
            );
        }
        for (unit, expected) in [
            ("nanosecond", "123456789"),
            ("microsecond", "123456"),
            ("millisecond", "123"),
            ("second", "56"),
        ] {
            assert_eq!(
                run(&format!("g.inject({value}).date_part(DT.{unit}).next()")),
                expected,
                "{unit}"
            );
        }
        // A time and a duration truncate and give their parts too.
        assert_eq!(
            run(r#"g.inject(time("23:59:59.999999999")).date_trunc(DT.microsecond).next()"#),
            r#"time("23:59:59.999999")"#
        );
        assert_eq!(
            run(r#"g.inject(duration("-PT1.999999999S")).date_trunc(DT.millisecond).next()"#),
            r#"duration("-PT1.999S")"#
        );
        assert_eq!(
            run(r#"g.inject(duration("-PT1.999999999S")).date_part(DT.nanosecond).next()"#),
            "-999999999"
        );
        // Adding nanoseconds wraps a time and moves a datetime; differences count them.
        assert_eq!(
            run(r#"g.inject(time("23:59:59.999999999")).date_add(DT.nanosecond, 2).next()"#),
            r#"time("00:00:00.000000001")"#
        );
        assert_eq!(
            run(r#"g.inject(datetime("2024-01-01T00:00:00Z")).date_add(DT.nanosecond, -1).next()"#),
            r#"datetime("2023-12-31T23:59:59.999999999Z")"#
        );
        assert_eq!(
            run(
                r#"g.inject(datetime("2024-01-01T00:00:00.000000005Z")).date_diff(datetime("2024-01-01T00:00:00Z"), DT.nanosecond).next()"#
            ),
            "5"
        );
        assert_eq!(
            run(
                r#"g.inject(localdatetime("2024-01-01T00:00:00")).date_add(duration("PT0.000000001S")).next()"#
            ),
            r#"localdatetime("2024-01-01T00:00:00.000000001")"#
        );
        // The range edges: an error, never a panic.
        for script in [
            r#"g.inject(localdatetime("9999-12-31T23:59:59.999999999")).date_add(DT.nanosecond, 1).next()"#,
            r#"g.inject(localdatetime("-9999-01-01T00:00:00")).date_add(DT.nanosecond, -1).next()"#,
            r#"g.inject(datetime("9999-12-31T23:59:59.999999999Z")).date_add(DT.nanosecond, 9223372036854775807).next()"#,
            r#"g.inject(duration("PT0S")).date_add(DT.hour, 9223372036854775807).next()"#,
        ] {
            assert!(
                fails(script).contains("outside the supported range"),
                "{script}"
            );
        }
        assert_eq!(
            run(
                r#"g.inject(localdatetime("9999-12-31T23:59:59.999999998")).date_add(DT.nanosecond, 1).next()"#
            ),
            r#"localdatetime("9999-12-31T23:59:59.999999999")"#
        );
    }

    #[test]
    fn a_property_index_tells_values_one_nanosecond_apart() {
        let graph = Arc::new(GraphSource::empty());
        let script = r#"
            g.create_index(#{label: "e", key: "at"});
            g.add_v("e").property("n", 1).property("at", time("12:00:00.000000001")).iterate();
            g.add_v("e").property("n", 2).property("at", time("12:00:00.000000002")).iterate();
            g.add_v("e").property("n", 3).property("at", localdatetime("2024-01-01T00:00:00.000000001")).iterate();
            g.add_v("e").property("n", 4).property("at", localdatetime("2024-01-01T00:00:00.000000002")).iterate();
            [
                g.v().has_label("e").has("at", time("12:00:00.000000002")).values("n").to_list(),
                g.v().has_label("e").has("at", localdatetime("2024-01-01T00:00:00.000000001")).values("n").to_list(),
                g.v().has_label("e").has("at", P.gt(localdatetime("2024-01-01T00:00:00.000000001"))).values("n").to_list(),
                g.v().has_label("e").has("at", P.lt(time("12:00:00.000000002"))).values("n").to_list(),
            ]
        "#;
        let value = eval_value(graph, script).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(debug_value(&value), "[[2], [3], [4], [1]]");
    }
}

#[cfg(feature = "gql")]
mod gql {
    use graphersal::prelude::*;

    fn single(query: &str) -> String {
        let graph = TraversalGraph::tinkerpop_modern();
        let rows = graph
            .traversal()
            .gql(query)
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        match rows.as_slice() {
            [ElementProperty::Map(map)] => format!("{:?}", map.values().next()),
            [ElementProperty::Object(map)] => match map.values().next() {
                Some(ElementProperty::Temporal(t)) => t.to_string(),
                Some(other) => format!("{other:?}"),
                None => String::new(),
            },
            other => panic!("{query}: {other:?}"),
        }
    }

    #[test]
    fn gql_keeps_nanoseconds() {
        for (query, expected) in [
            (
                "RETURN localtime({hour: 12, minute: 31, second: 14, nanosecond: 645876123}) AS x",
                "12:31:14.645876123",
            ),
            (
                "RETURN localtime('12:31:14.645876123') AS x",
                "12:31:14.645876123",
            ),
            (
                "RETURN datetime({epochSeconds: 1, nanosecond: 23}) AS x",
                "1970-01-01T00:00:01.000000023Z",
            ),
            ("RETURN duration({nanoseconds: 1}) AS x", "PT0.000000001S"),
            ("RETURN duration('PT1S') / 3 AS x", "PT0.333333333S"),
            (
                "RETURN localtime.truncate('microsecond', localtime('12:31:14.645876123')) AS x",
                "12:31:14.645876",
            ),
            (
                "RETURN localtime('12:31:14.645876123').nanosecond AS x",
                "645876123",
            ),
            (
                "RETURN toString(localtime('12:31:14.645876123')) AS x",
                "12:31:14.645876123",
            ),
            (
                "RETURN duration.between(localtime('12:00'), localtime('12:00:00.000000001')) AS x",
                "PT0.000000001S",
            ),
        ] {
            let got = single(query);
            assert!(
                got.contains(expected),
                "{query}: got {got}, expected {expected}"
            );
        }
    }
}

#[cfg(feature = "persist")]
mod persist {
    use graphersal::persist::{MemDir, Store, StoreOptions};
    use graphersal::prelude::*;
    use graphersal::temporal::{Duration, Temporal, Time};

    fn add(store: &Store, id: &str, value: ElementProperty) -> Result<(), GraphError> {
        let mut map = PropertyMap::default();
        map.insert("at", value);
        store.graph().write().transaction(|g| {
            g.add_vertex(Some(id.into()), vec!["e".into()], map)?;
            Ok::<_, GraphError>(())
        })
    }

    #[test]
    fn a_store_keeps_a_value_below_the_microsecond() {
        let dir = MemDir::new();
        let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
        let whole = Time::from_hms_micro(12, 0, 0, 1).unwrap();
        add(
            &store,
            "ok",
            ElementProperty::Temporal(Temporal::Time(whole)),
        )
        .unwrap();
        let values = [
            Temporal::Time(Time::from_hms_nano(12, 0, 0, 1).unwrap()),
            Temporal::Duration(Duration::from_nanos(1).unwrap()),
            Temporal::LocalDateTime("2024-01-01T00:00:00.000000001".parse().unwrap()),
            Temporal::DateTime("2024-01-01T00:00:00.000000001Z".parse().unwrap()),
        ];
        for (index, value) in values.iter().enumerate() {
            add(
                &store,
                &format!("v{index}"),
                ElementProperty::Temporal(*value),
            )
            .expect("a value below the microsecond is stored");
            // Nested in an array too.
            let nested = ElementProperty::Array(vec![ElementProperty::Temporal(*value)]);
            add(&store, &format!("n{index}"), nested).unwrap();
        }
        // Through a query.
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("e")
            .property("id_", "query")
            .property(
                "at",
                ElementProperty::Temporal(Temporal::Time(Time::from_hms_nano(1, 0, 0, 5).unwrap())),
            )
            .iterate()
            .unwrap();
        let expect = |lock: &TraversalGraph| {
            assert_eq!(lock.vertex_count(), 1 + 2 * values.len() + 1);
            for (index, value) in values.iter().enumerate() {
                let v = lock
                    .get_vertex_by_id(format!("v{index}").as_str())
                    .unwrap()
                    .unwrap();
                let got = lock
                    .vertex_properties(v)
                    .unwrap()
                    .unwrap()
                    .get("at")
                    .unwrap();
                let got = got.as_temporal().unwrap();
                assert_eq!(
                    format!("{}({got})", got.kind()),
                    format!("{}({value})", value.kind())
                );
            }
        };
        // WAL replay, then a checkpoint and a load from the snapshot.
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        expect(&store.graph().read());
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        expect(&store.graph().read());
        let values = store
            .graph()
            .read()
            .traversal()
            .v(None)
            .has("id_", "query")
            .values("at")
            .to_list()
            .unwrap();
        assert_eq!(
            values[0].as_temporal().unwrap().to_string(),
            "01:00:00.000000005"
        );
    }
}

#[cfg(feature = "serde")]
#[test]
fn schema_coercion_keeps_nanoseconds() {
    let schema = GraphSchema::from_json(
        r#"{"mode": "open", "vertices": {"e": {"schema": {"type": "object", "properties": {
            "t": {"type": "string", "format": "time"},
            "at": {"type": "string", "format": "date-time"},
            "d": {"type": "string", "format": "duration"}
        }}}}}"#,
    )
    .unwrap();
    let mut graph = TraversalGraph::new();
    graph.set_schema(schema).unwrap();
    graph
        .traversal_mut()
        .add_v("e")
        .property("t", "12:00:00.000000001")
        .property("at", "2024-01-01T00:00:00.123456789+01:00")
        .property("d", "PT0.000000001S")
        .iterate()
        .unwrap();
    let read = |key: &str| {
        graph.traversal().v(None).values(key).to_list().unwrap()[0]
            .as_temporal()
            .map(|t| t.to_string())
    };
    assert_eq!(read("t").as_deref(), Some("12:00:00.000000001"));
    assert_eq!(
        read("at").as_deref(),
        Some("2024-01-01T00:00:00.123456789+01:00")
    );
    assert_eq!(read("d").as_deref(), Some("PT0.000000001S"));
    let _ = DT::Nanosecond;
}
