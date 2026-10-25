//! Temporal v2, stage 2: the time of day with a UTC offset (`OffsetTime`, Rhai `offsettime`,
//! GQL `zoned_time()` / Cypher compatibility `time()`) and zone names kept in a `datetime`
//! (`..+02:00[Europe/Berlin]`): text, comparison, daylight-saving-correct arithmetic, the
//! schema format `offset-time`, the property index, GraphSON, the date steps, GQL, and a Store
//! that keeps both forms (file format version 3; stage 2 refused them under version 2).

use graphersal::prelude::*;
use graphersal::temporal::{
    DT, DateTime, Duration, LocalDateTime, OffsetTime, Temporal, TemporalKind, Time,
};

fn dt(text: &str) -> DateTime {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn ot(text: &str) -> OffsetTime {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

#[test]
fn the_values_keep_their_size() {
    assert_eq!(std::mem::size_of::<Temporal>(), 24);
    assert_eq!(std::mem::size_of::<ElementProperty>(), 32);
    assert!(std::mem::size_of::<DateTime>() <= 20);
    assert_eq!(std::mem::size_of::<OffsetTime>(), 16);
}

#[test]
fn an_offset_time_parses_prints_and_compares_by_its_utc_time() {
    assert_eq!(ot("10:00:00.5+02:00").to_string(), "10:00:00.500+02:00");
    assert_eq!(ot("10:00Z").to_string(), "10:00:00Z");
    assert_eq!(
        ot("10:00:00.000000001-0530").to_string(),
        "10:00:00.000000001-05:30"
    );
    assert_eq!(ot("23:59:59+18").to_string(), "23:59:59+18:00");
    // The offset is required; a zone name and an offset beyond ±18:00 are refused.
    for text in [
        "10:00",
        "10:00[Europe/Berlin]",
        "10:00+19:00",
        "25:00Z",
        "10:00+02:00x",
    ] {
        assert!(text.parse::<OffsetTime>().is_err(), "{text}");
    }
    // A time has no offset: the parsers stay apart (rule 7: no guessing).
    assert!("10:00+02:00".parse::<Time>().is_err());
    // Equal UTC times are equal (the offset is shown, never compared), ordered by UTC time.
    assert_eq!(ot("12:00+01:00"), ot("11:00Z"));
    assert!(ot("12:00+01:00") < ot("11:30Z"));
    assert!(ot("01:00+02:00") < ot("00:00Z"));
    let value = ot("12:00+01:00");
    assert_eq!(value.time().hour(), 12);
    assert_eq!(value.offset_seconds(), 3600);
    assert_eq!(
        value.with_offset(-3600).unwrap().to_string(),
        "10:00:00-01:00"
    );
    assert_eq!(
        ot("23:00Z").with_offset(7200).unwrap().to_string(),
        "01:00:00+02:00"
    );
    assert!(OffsetTime::new(Time::from_hms_nano(1, 0, 0, 0).unwrap(), 18 * 3600 + 1).is_err());
    // Its own kind and the order after the other kinds.
    let t = Temporal::OffsetTime(value);
    assert_eq!(t.kind(), TemporalKind::OffsetTime);
    assert_eq!(t.kind().name(), "offsettime");
    assert!(Temporal::Duration(Duration::from_micros(1)) < t);
    assert_eq!(
        Temporal::parse_as(TemporalKind::OffsetTime, "12:00+01:00").unwrap(),
        t
    );
}

#[test]
fn a_zone_name_is_kept_printed_and_never_compared() {
    let berlin = dt("2024-03-30T12:00[Europe/Berlin]");
    assert_eq!(
        berlin.to_string(),
        "2024-03-30T12:00:00+01:00[Europe/Berlin]"
    );
    assert_eq!(berlin.zone_name(), Some("Europe/Berlin"));
    assert_eq!(berlin.offset_seconds(), 3600);
    // The name is matched in any case and kept as the database spells it.
    assert_eq!(
        dt("2024-03-30T12:00[europe/berlin]").zone_name(),
        Some("Europe/Berlin")
    );
    // The same instant with or without a name, at another offset: equal.
    assert_eq!(berlin, dt("2024-03-30T11:00Z"));
    assert_eq!(
        berlin.without_zone().to_string(),
        "2024-03-30T12:00:00+01:00"
    );
    // A fixed offset in brackets is no name; an offset that does not match the zone is refused.
    assert_eq!(dt("2024-03-30T12:00[+01:00]").zone_name(), None);
    assert!(
        "2024-07-30T12:00+01:00[Europe/Berlin]"
            .parse::<DateTime>()
            .is_err()
    );
    // A zone of zero offset prints Z before the name (`java.time`).
    assert_eq!(
        dt("2024-01-01T10:00[Europe/London]").to_string(),
        "2024-01-01T10:00:00Z[Europe/London]"
    );
    // Moving into a zone keeps the instant and the name; `with_offset` drops the name.
    let ny = berlin.in_zone("America/New_York").unwrap();
    assert_eq!(
        ny.to_string(),
        "2024-03-30T07:00:00-04:00[America/New_York]"
    );
    assert_eq!(ny.with_offset(0).unwrap().zone_name(), None);
    assert_eq!(berlin.in_zone("+05:00").unwrap().zone_name(), None);
    assert!(berlin.in_zone("Mars/Olympus").is_err());
    let local: LocalDateTime = "2024-03-31T02:30".parse().unwrap();
    // A skipped wall-clock time moves forward by the gap.
    assert_eq!(
        DateTime::from_local_in(local, "Europe/Berlin")
            .unwrap()
            .to_string(),
        "2024-03-31T03:30:00+02:00[Europe/Berlin]"
    );
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
    fn calendar_arithmetic_of_a_zoned_value_follows_daylight_saving() {
        for (script, expected) in [
            // A day stays a calendar day across the spring-forward change (23 hours).
            (
                r#"datetime("2024-03-30T12:00[Europe/Berlin]") + duration("P1D")"#,
                r#"datetime("2024-03-31T12:00:00+02:00[Europe/Berlin]")"#,
            ),
            // A time part moves the instant.
            (
                r#"datetime("2024-03-30T12:00[Europe/Berlin]") + duration("PT24H")"#,
                r#"datetime("2024-03-31T13:00:00+02:00[Europe/Berlin]")"#,
            ),
            // The same without a name: the fixed offset stays.
            (
                r#"datetime("2024-03-30T12:00+01:00") + duration("P1D")"#,
                r#"datetime("2024-03-31T12:00:00+01:00")"#,
            ),
            (
                r#"g.inject(datetime("2024-10-26T12:00[Europe/Berlin]")).date_add(DT.day, 1).next()"#,
                r#"datetime("2024-10-27T12:00:00+01:00[Europe/Berlin]")"#,
            ),
            (
                r#"g.inject(datetime("2024-03-30T12:00[Europe/Berlin]")).date_add(DT.month, 1).next()"#,
                r#"datetime("2024-04-30T12:00:00+02:00[Europe/Berlin]")"#,
            ),
            (
                r#"g.inject(datetime("2024-03-31T12:30[Europe/Berlin]")).date_trunc(DT.day).next()"#,
                r#"datetime("2024-03-31T00:00:00+01:00[Europe/Berlin]")"#,
            ),
            (
                r#"g.inject(datetime("2024-03-31T12:00[Europe/Berlin]")).date_diff(datetime("2024-03-30T12:00[Europe/Berlin]"), DT.day).next()"#,
                "1",
            ),
            (
                r#"g.inject(datetime("2024-03-31T12:00[Europe/Berlin]")).date_diff(datetime("2024-03-30T12:00[Europe/Berlin]"), DT.hour).next()"#,
                "23",
            ),
            // A zone argument wins over the value's own zone; the result has no name.
            (
                r#"g.inject(datetime("2024-03-30T12:00[Europe/Berlin]")).date_add(DT.day, 1, "+01:00").next()"#,
                r#"datetime("2024-03-31T12:00:00+01:00")"#,
            ),
            (
                r#"datetime("2024-03-30T12:00[Europe/Berlin]").timezone"#,
                r#""Europe/Berlin""#,
            ),
            (
                r#"datetime("2024-03-30T12:00+01:00").timezone"#,
                r#""+01:00""#,
            ),
        ] {
            assert_eq!(run(script), expected, "{script}");
        }
    }

    #[test]
    fn offsettime_in_scripts() {
        for (script, expected) in [
            (
                r#"offsettime("10:00:00.5+02:00")"#,
                r#"offsettime("10:00:00.500+02:00")"#,
            ),
            (r#"offsettime("10:00Z")"#, r#"offsettime("10:00:00Z")"#),
            (
                r#"offsettime("10:00+02:00") == offsettime("08:00Z")"#,
                "true",
            ),
            (
                r#"offsettime("10:00+02:00") < offsettime("08:30Z")"#,
                "true",
            ),
            (
                r#"offsettime("23:30+02:00") + duration("PT1H")"#,
                r#"offsettime("00:30:00+02:00")"#,
            ),
            (
                r#"offsettime("10:00+02:00") - offsettime("09:00Z")"#,
                r#"duration("-PT1H")"#,
            ),
            (r#"offsettime("10:15:30.25+02:00").hour"#, "10"),
            (r#"offsettime("10:15:30.25+02:00").nanosecond"#, "250000000"),
            (r#"offsettime("10:15:30+02:00").offset"#, "7200"),
            (r#"offsettime("10:15:30+02:00").timezone"#, r#""+02:00""#),
            (
                r#"offsettime("10:15:30+02:00").as_string()"#,
                r#""10:15:30+02:00""#,
            ),
            (
                r#"type_of(offsettime("10:15:30+02:00"))"#,
                r#""offsettime""#,
            ),
            (
                r#"g.inject(offsettime("23:30+02:00")).date_add(DT.hour, 1).next()"#,
                r#"offsettime("00:30:00+02:00")"#,
            ),
            (
                r#"g.inject(offsettime("23:31:15+02:00")).date_trunc(DT.hour).next()"#,
                r#"offsettime("23:00:00+02:00")"#,
            ),
            (
                r#"g.inject(offsettime("23:30+02:00")).date_part(DT.offset).next()"#,
                "7200",
            ),
            (
                r#"g.inject(offsettime("23:30+02:00")).date_part(DT.minute).next()"#,
                "30",
            ),
            (
                r#"g.inject(offsettime("10:00+02:00")).date_diff(offsettime("07:00Z"), DT.minute).next()"#,
                "60",
            ),
            (
                r#"g.inject(datetime("2024-03-30T12:30[Europe/Berlin]")).as_date(GType.OFFSETTIME).next()"#,
                r#"offsettime("12:30:00+01:00")"#,
            ),
            (
                r#"g.inject(time("12:30")).as_date(GType.OFFSETTIME).next()"#,
                r#"offsettime("12:30:00Z")"#,
            ),
            (
                r#"g.inject("12:30+05:00").as_date(GType.OFFSETTIME).next()"#,
                r#"offsettime("12:30:00+05:00")"#,
            ),
            (
                r#"g.inject(offsettime("12:30+05:00")).as_date(GType.TIME).next()"#,
                r#"time("12:30:00")"#,
            ),
            (
                r#"g.inject(offsettime("12:30+05:00"), time("12:30")).is(P.typeOf(GType.OFFSETTIME)).count().next()"#,
                "1",
            ),
            (
                r#"g.inject(offsettime("12:30+05:00")).is(P.typeOf("OffsetTime")).count().next()"#,
                "1",
            ),
            (r#"GType.OFFSETTIME"#, "GType.OFFSETTIME"),
        ] {
            assert_eq!(run(script), expected, "{script}");
        }
        // An offset time has no date: date units, days in a duration and a zone are refused.
        for script in [
            r#"g.inject(offsettime("10:00Z")).date_add(DT.day, 1).next()"#,
            r#"offsettime("10:00Z") + duration("P1D")"#,
            r#"g.inject(offsettime("10:00Z")).date_add(DT.hour, 1, "Europe/Berlin").next()"#,
            r#"g.inject(offsettime("10:00Z")).as_date(GType.DATETIME).next()"#,
            r#"offsettime("10:00")"#,
        ] {
            fails(script);
        }
    }

    #[test]
    fn a_property_index_answers_offset_times_by_their_utc_time() {
        let graph = Arc::new(GraphSource::empty());
        let script = r#"
            g.create_index(#{label: "e", key: "at"});
            g.add_v("e").property("n", 1).property("at", offsettime("10:00+02:00")).iterate();
            g.add_v("e").property("n", 2).property("at", offsettime("09:00Z")).iterate();
            g.add_v("e").property("n", 3).property("at", time("08:00")).iterate();
            g.add_v("e").property("n", 4).property("at", datetime("2024-01-01T10:00[Europe/Berlin]")).iterate();
            [
                g.v().has_label("e").has("at", offsettime("08:00Z")).values("n").to_list(),
                g.v().has_label("e").has("at", P.gt(offsettime("08:30Z"))).values("n").to_list(),
                g.v().has_label("e").has("at", datetime("2024-01-01T09:00Z")).values("n").to_list(),
            ]
        "#;
        let value = eval_value(graph, script).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(debug_value(&value), "[[1], [2], [4]]");
    }
}

#[cfg(feature = "gql")]
mod gql {
    use graphersal::prelude::*;

    fn single(query: &str) -> String {
        let graph = TraversalGraph::new();
        let rows = graph
            .traversal()
            .gql(query)
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        let value = match rows.as_slice() {
            [ElementProperty::Object(map)] => map.values().next().cloned(),
            [ElementProperty::Map(map)] => map.values().next().cloned(),
            other => panic!("{query}: {other:?}"),
        };
        match value {
            Some(ElementProperty::Temporal(t)) => t.to_string(),
            Some(ElementProperty::String(s)) => s.to_string(),
            Some(other) => format!("{other:?}"),
            None => String::new(),
        }
    }

    #[test]
    fn zoned_time_and_zone_names() {
        for (query, expected) in [
            (
                "RETURN time({hour: 12, timezone: '+01:00'}) AS x",
                "12:00:00+01:00",
            ),
            ("RETURN zoned_time('214032.142Z') AS x", "21:40:32.142Z"),
            ("RETURN time('12:00') AS x", "12:00:00Z"),
            (
                "RETURN time({time: time('12:31:14+01:00'), timezone: '+05:00'}) AS x",
                "16:31:14+05:00",
            ),
            ("RETURN time(localtime('12:31')) AS x", "12:31:00Z"),
            ("RETURN localtime(time('12:31+05:00')) AS x", "12:31:00"),
            ("RETURN toString(time('12:31+05:00')) AS x", "12:31+05:00"),
            ("RETURN time('12:31+05:00').offsetMinutes AS x", "300"),
            (
                "RETURN time('12:31:14.000000001+01:00') + duration({hours: 16}) AS x",
                "04:31:14.000000001+01:00",
            ),
            (
                "RETURN time.truncate('hour', time('12:31-01:00'), {timezone: '+01:00'}) AS x",
                "12:00:00+01:00",
            ),
            (
                "RETURN duration.between(time('14:30'), time('16:30+01:00')) AS x",
                "PT1H",
            ),
            (
                "RETURN datetime({year: 1984, month: 10, day: 11, hour: 12, timezone: 'Europe/Stockholm'}) AS x",
                "1984-10-11T12:00:00+01:00[Europe/Stockholm]",
            ),
            (
                "RETURN datetime('2015-07-21T21:40:32.142[Europe/London]').timezone AS x",
                "Europe/London",
            ),
            (
                "RETURN toString(datetime('2017-08-08T12:31:14.645876123[Europe/Stockholm]')) AS x",
                "2017-08-08T12:31:14.645876123+02:00[Europe/Stockholm]",
            ),
            (
                "RETURN datetime('2024-03-30T12:00[Europe/Berlin]') + duration('P1D') AS x",
                "2024-03-31T12:00:00+02:00[Europe/Berlin]",
            ),
            (
                "RETURN duration.inSeconds(datetime({year: 2017, month: 10, day: 29, hour: 0, timezone: 'Europe/Stockholm'}), localdatetime({year: 2017, month: 10, day: 29, hour: 4})) AS x",
                "PT5H",
            ),
            (
                "RETURN datetime.truncate('day', datetime('2024-03-31T12:30[Europe/Berlin]')) AS x",
                "2024-03-31T00:00:00+01:00[Europe/Berlin]",
            ),
        ] {
            assert_eq!(single(query), expected, "{query}");
        }
    }
}

#[cfg(feature = "serde")]
#[test]
fn the_schema_declares_and_coerces_an_offset_time() {
    use graphersal::schema::SchemaFormat;
    assert_eq!(SchemaFormat::OffsetTime.as_str(), "offset-time");
    assert_eq!(
        SchemaFormat::from_name("offset-time"),
        Some(SchemaFormat::OffsetTime)
    );
    assert_eq!(
        SchemaFormat::OffsetTime.temporal_kind(),
        Some(TemporalKind::OffsetTime)
    );
    let schema = GraphSchema::from_json(
        r#"{"mode": "open", "vertices": {"e": {"schema": {"type": "object", "properties": {
            "t": {"type": "string", "format": "offset-time", "formatMinimum": "08:00Z"}
        }}}}}"#,
    )
    .unwrap();
    let mut graph = TraversalGraph::new();
    graph.set_schema(schema).unwrap();
    let mut props = PropertyMap::default();
    props.insert("t", ElementProperty::from("10:00+01:00".to_string()));
    let v = graph
        .transaction(|g| g.add_vertex(None, vec!["e".into()], props))
        .unwrap();
    assert_eq!(
        graph.vertex_properties(v).unwrap().get("t"),
        Some(&ElementProperty::Temporal(Temporal::OffsetTime(
            "10:00+01:00".parse().unwrap()
        )))
    );
    // A time without its offset, and one below the minimum (07:30Z), are refused.
    for text in ["10:00", "08:30+01:00"] {
        let mut props = PropertyMap::default();
        props.insert("t", ElementProperty::from(text.to_string()));
        assert!(
            graph
                .transaction(|g| g.add_vertex(None, vec!["e".into()], props))
                .is_err(),
            "{text}"
        );
    }
}

#[cfg(feature = "io")]
#[test]
fn graphson_round_trips_both_forms() {
    use graphersal::io::GraphSONImport;
    let mut source = TraversalGraph::new();
    let mut props = PropertyMap::default();
    props.insert(
        "t",
        Temporal::OffsetTime(ot("10:00:00.000000001+02:00")).into(),
    );
    props.insert(
        "z",
        Temporal::DateTime(dt("2024-03-31T12:00[Europe/Berlin]")).into(),
    );
    source
        .transaction(|g| g.add_vertex(Some("v".into()), vec!["e".into()], props))
        .unwrap();
    let graph = Graph::new(source);
    let mut out = Vec::new();
    graph.export_graphson_writer(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains(r#"{"@type":"gx:OffsetTime","@value":"10:00:00.000000001+02:00"}"#),
        "{text}"
    );
    assert!(
        text.contains(
            r#"{"@type":"gx:ZonedDateTime","@value":"2024-03-31T12:00:00+02:00[Europe/Berlin]"}"#
        ),
        "{text}"
    );
    let mut back = TraversalGraph::new();
    let report = GraphSONImport::new()
        .import_reader(&mut back, text.as_bytes())
        .unwrap();
    assert!(report.notes.is_empty(), "{report}");
    let v = back.get_vertex_by_id("v").unwrap();
    let props = back.vertex_properties(v).unwrap();
    let z = props
        .get("z")
        .and_then(ElementProperty::as_temporal)
        .unwrap();
    assert_eq!(z.to_string(), "2024-03-31T12:00:00+02:00[Europe/Berlin]");
    let t = props
        .get("t")
        .and_then(ElementProperty::as_temporal)
        .unwrap();
    assert_eq!(t.to_string(), "10:00:00.000000001+02:00");
}

#[cfg(feature = "persist")]
mod persist {
    use graphersal::persist::{MemDir, Store, StoreOptions};
    use graphersal::prelude::*;
    use graphersal::temporal::Temporal;

    fn add(store: &Store, id: &str, value: ElementProperty) -> Result<(), GraphError> {
        let mut map = PropertyMap::default();
        map.insert("at", value);
        store.graph().write().transaction(|g| {
            g.add_vertex(Some(id.into()), vec!["e".into()], map)?;
            Ok::<_, GraphError>(())
        })
    }

    /// Every field of a value as text: an offset time's offset and a datetime's zone name
    /// (equality compares the instant only).
    fn exact(value: &ElementProperty) -> String {
        match value {
            ElementProperty::Temporal(t) => format!("{}({t})", t.kind()),
            ElementProperty::Array(items) => {
                let items: Vec<String> = items.iter().map(exact).collect();
                format!("[{}]", items.join(", "))
            }
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn a_store_keeps_an_offset_time_and_a_zone_name() {
        let dir = MemDir::new();
        let store = Store::create_with(&dir, TraversalGraph::new(), StoreOptions::new()).unwrap();
        let values = [
            Temporal::DateTime("2024-01-01T10:00:00+01:00".parse().unwrap()),
            Temporal::OffsetTime("10:00+02:00".parse().unwrap()),
            Temporal::DateTime("2024-01-01T10:00[Europe/Berlin]".parse().unwrap()),
            Temporal::DateTime(
                "2024-07-01T10:00:00.000000001[America/Argentina/ComodRivadavia]"
                    .parse()
                    .unwrap(),
            ),
        ];
        let mut expected = Vec::new();
        for (index, value) in values.iter().enumerate() {
            let plain = ElementProperty::Temporal(*value);
            let nested = ElementProperty::Array(vec![ElementProperty::Temporal(*value)]);
            add(&store, &format!("v{index}"), plain.clone()).unwrap();
            add(&store, &format!("n{index}"), nested.clone()).unwrap();
            expected.push((format!("v{index}"), exact(&plain)));
            expected.push((format!("n{index}"), exact(&nested)));
        }
        // Through a query.
        store
            .graph()
            .write()
            .traversal_mut()
            .add_v("e")
            .property(
                "at",
                ElementProperty::Temporal(Temporal::OffsetTime("10:00Z".parse().unwrap())),
            )
            .iterate()
            .unwrap();
        let check = |store: &Store| {
            let lock = store.graph().read();
            assert_eq!(lock.vertex_count(), expected.len() + 1);
            for (id, text) in &expected {
                let v = lock.get_vertex_by_id(id.as_str()).unwrap();
                assert_eq!(
                    &exact(lock.vertex_properties(v).unwrap().get("at").unwrap()),
                    text
                );
            }
        };
        check(&store);
        // WAL replay, then a checkpoint and a load from the snapshot.
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        check(&store);
        store.checkpoint(None).unwrap();
        store.close().unwrap();
        let store = Store::open(&dir, StoreOptions::new()).unwrap();
        check(&store);
        assert!(
            exact(&ElementProperty::Temporal(values[2])).contains("[Europe/Berlin]"),
            "the zone name prints"
        );
    }
}

#[test]
fn rust_date_steps_take_offset_times() {
    let graph = TraversalGraph::new();
    let value = ElementProperty::Temporal(Temporal::OffsetTime(ot("23:30+02:00")));
    let got = graph
        .traversal()
        .inject(vec![value])
        .date_add(DT::Minute, 45)
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(got.as_temporal().unwrap().to_string(), "00:15:00+02:00");
}
