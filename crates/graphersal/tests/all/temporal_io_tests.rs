//! Temporal v2, stage 4: every temporal kind at its range edges, with nanoseconds, zone names,
//! offset times and the longest durations, through every text exchange format: GraphSON export
//! and import (line form, wrapped document form, live import, a `.jsonl` file, the script
//! functions), the `java.time` texts TinkerPop writes, GraphML with a declared schema, and the
//! serialized `ChangeSet`.

#![cfg(feature = "io")]

use graphersal::io::{GraphExport, GraphImport, GraphSONImport, ImportNoteKind, ImportReport};
use graphersal::prelude::*;
use graphersal::temporal::{Temporal, TemporalKind};

/// `(key, kind, canonical text)` of every value the round trips carry: each kind at both range
/// edges with nine fraction digits, zone names (one on a repeated wall-clock hour), offsets
/// with seconds, offset times at the widest offsets, and durations at the `java.time.Duration`
/// edges, a period at the month edge, and an interval of all three parts.
const VALUES: &[(&str, TemporalKind, &str)] = &[
    ("date_min", TemporalKind::Date, "-9999-01-01"),
    ("date_max", TemporalKind::Date, "9999-12-31"),
    (
        "local_min",
        TemporalKind::LocalDateTime,
        "-9999-01-01T00:00:00",
    ),
    (
        "local_max",
        TemporalKind::LocalDateTime,
        "9999-12-31T23:59:59.999999999",
    ),
    (
        "dt_min",
        TemporalKind::DateTime,
        "-9999-01-01T00:00:00.000000001Z",
    ),
    (
        "dt_max",
        TemporalKind::DateTime,
        "9999-12-31T23:59:59.999999999Z",
    ),
    (
        "dt_offset_seconds",
        TemporalKind::DateTime,
        "2007-12-03T10:15:30.000000500+03:06:09",
    ),
    (
        "dt_zoned",
        TemporalKind::DateTime,
        "2024-03-31T03:30:00.123456789+02:00[Europe/Berlin]",
    ),
    (
        "dt_zoned_fold",
        TemporalKind::DateTime,
        "2024-10-27T02:30:00.000000001+01:00[Europe/Berlin]",
    ),
    (
        "dt_utc_zone",
        TemporalKind::DateTime,
        "2016-12-23T12:12:24Z[UTC]",
    ),
    ("time_min", TemporalKind::Time, "00:00:00"),
    ("time_max", TemporalKind::Time, "23:59:59.999999999"),
    (
        "ot_east",
        TemporalKind::OffsetTime,
        "23:59:59.999999999+18:00",
    ),
    ("ot_west", TemporalKind::OffsetTime, "00:00:00-18:00"),
    (
        "ot_quarter",
        TemporalKind::OffsetTime,
        "10:00:00.000000001+05:45",
    ),
    (
        "dur_max",
        TemporalKind::Duration,
        "PT2562047788015215H30M7.999999999S",
    ),
    (
        "dur_min",
        TemporalKind::Duration,
        "-PT2562047788015215H30M8S",
    ),
    ("dur_tiny", TemporalKind::Duration, "-PT0.000000001S"),
    ("dur_zero", TemporalKind::Duration, "PT0S"),
    (
        "period_max",
        TemporalKind::Duration,
        "P178956970Y7M2147483647D",
    ),
    ("period_neg", TemporalKind::Duration, "-P178956970Y8M"),
    ("interval", TemporalKind::Duration, "P1M-2DT0.000000003S"),
];

fn value(kind: TemporalKind, text: &str) -> ElementProperty {
    ElementProperty::Temporal(Temporal::parse_strict(kind, text).unwrap())
}

/// Every field of a value, recursively (a `datetime`'s offset and zone name, a `duration` field
/// by field): equality compares a `datetime` by its instant only.
fn exact(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Temporal(t) => format!("{}({t})", t.kind()),
        ElementProperty::Array(items) => {
            let items: Vec<String> = items.iter().map(exact).collect();
            format!("[{}]", items.join(", "))
        }
        ElementProperty::Object(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{key}: {}", exact(item)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        other => format!("{other:?}"),
    }
}

/// The property map of the round trips: every value, a list of all of them and an object
/// nesting a few.
fn properties() -> PropertyMap {
    let mut props = PropertyMap::default();
    for (key, kind, text) in VALUES {
        props.insert(*key, value(*kind, text));
    }
    props.insert(
        "all",
        ElementProperty::Array(
            VALUES
                .iter()
                .map(|(_, kind, text)| value(*kind, text))
                .collect(),
        ),
    );
    let mut nested = PropertyMap::default();
    nested.insert("at", value(TemporalKind::DateTime, VALUES[7].2));
    nested.insert("t", value(TemporalKind::OffsetTime, VALUES[12].2));
    nested.insert("d", value(TemporalKind::Duration, VALUES[16].2));
    props.insert("nested", ElementProperty::Object(Box::new(nested)));
    props
}

fn expected() -> Vec<String> {
    properties()
        .iter()
        .map(|(key, value)| format!("{key} = {}", exact(value)))
        .collect()
}

fn source() -> Graph {
    let mut graph = TraversalGraph::new();
    let mut edge_props = PropertyMap::default();
    edge_props.insert("since", value(TemporalKind::DateTime, VALUES[8].2));
    edge_props.insert("took", value(TemporalKind::Duration, VALUES[15].2));
    graph
        .transaction(|g| {
            let a = g.add_vertex(Some("a".into()), vec!["event".into()], properties())?;
            let b = g.add_vertex(
                Some("b".into()),
                vec!["event".into()],
                PropertyMap::default(),
            )?;
            g.add_edge(Some("e".into()), Some("next".into()), a, b, edge_props)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
    Graph::new(graph)
}

fn vertex_values<S: GraphStorage>(graph: &S, id: &str) -> Vec<String> {
    let v = graph.get_vertex_by_id(id).expect("vertex");
    graph
        .vertex_properties(v)
        .unwrap()
        .iter()
        .map(|(key, value)| format!("{key} = {}", exact(value)))
        .collect()
}

fn edge_values<S: GraphStorage>(graph: &S) -> Vec<String> {
    let e = graph.get_edge_by_id("e").expect("edge");
    let mut values: Vec<String> = graph
        .edge_properties(e)
        .unwrap()
        .iter()
        .map(|(key, value)| format!("{key} = {}", exact(value)))
        .collect();
    values.sort();
    values
}

fn assert_round_trip<S: GraphStorage>(graph: &S, report: &ImportReport) {
    assert!(report.notes.is_empty(), "{report}");
    assert_eq!(vertex_values(graph, "a"), expected());
    assert_eq!(
        edge_values(graph),
        vec![
            format!("since = datetime({})", VALUES[8].2),
            format!("took = duration({})", VALUES[15].2),
        ]
    );
}

fn export_graphson(graph: &Graph) -> String {
    let mut out = Vec::new();
    graph.export_graphson_writer(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "graphersal-temporal-io-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------------------------
// GraphSON
// ---------------------------------------------------------------------------------------------

#[test]
fn graphson_export_writes_the_java_time_tag_of_every_kind() {
    let text = export_graphson(&source());
    for part in [
        r#"{"@type":"gx:LocalDate","@value":"-9999-01-01"}"#,
        r#"{"@type":"gx:LocalDateTime","@value":"9999-12-31T23:59:59.999999999"}"#,
        r#"{"@type":"gx:OffsetDateTime","@value":"-9999-01-01T00:00:00.000000001Z"}"#,
        r#"{"@type":"gx:OffsetDateTime","@value":"2007-12-03T10:15:30.000000500+03:06:09"}"#,
        r#"{"@type":"gx:ZonedDateTime","@value":"2024-10-27T02:30:00.000000001+01:00[Europe/Berlin]"}"#,
        r#"{"@type":"gx:ZonedDateTime","@value":"2016-12-23T12:12:24Z[UTC]"}"#,
        r#"{"@type":"gx:LocalTime","@value":"23:59:59.999999999"}"#,
        r#"{"@type":"gx:OffsetTime","@value":"00:00:00-18:00"}"#,
        r#"{"@type":"gx:Duration","@value":"PT2562047788015215H30M7.999999999S"}"#,
        // A negative `gx:Duration` as `java.time` writes it: `Duration.parse` cannot read
        // `-PT2562047788015215H30M8S` (the fields are summed before the sign is applied).
        r#"{"@type":"gx:Duration","@value":"PT-2562047788015215H-30M-8S"}"#,
        r#"{"@type":"gx:Duration","@value":"PT-0.000000001S"}"#,
        r#"{"@type":"gx:Duration","@value":"PT0S"}"#,
        r#"{"@type":"gx:Period","@value":"P178956970Y7M2147483647D"}"#,
        r#"{"@type":"gx:Period","@value":"-P178956970Y8M"}"#,
        r#"{"@type":"graphersal:Interval","@value":"P1M-2DT0.000000003S"}"#,
    ] {
        assert!(text.contains(part), "{part} missing in {text}");
    }
}

#[test]
fn graphson_line_form_round_trips_exactly() {
    let text = export_graphson(&source());
    let mut back = TraversalGraph::new();
    let report = GraphSONImport::new()
        .import_reader(&mut back, text.as_bytes())
        .unwrap();
    assert_round_trip(&back, &report);
}

#[test]
fn graphson_wrapped_document_form_round_trips_exactly() {
    let text = export_graphson(&source());
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    let document = format!("{{\"vertices\": [\n{}\n]}}", lines.join(",\n"));
    let (back, report) = GraphSONImport::new().load(document.as_bytes()).unwrap();
    assert_round_trip(&*back.read(), &report);
}

#[test]
fn graphson_live_import_into_a_graph_round_trips_exactly() {
    let text = export_graphson(&source());
    let mut target = GraphSource::empty();
    let report = target.import_graphson_reader(text.as_bytes()).unwrap();
    assert_round_trip(&*target.read(), &report);
    // A second export of the imported graph is byte-identical.
    assert_eq!(export_graphson(&target), text);
}

#[test]
fn graphson_jsonl_file_round_trips_exactly() {
    let dir = scratch("jsonl");
    let path = dir.join("events.jsonl");
    source().export_graphson(path.to_str().unwrap()).unwrap();
    // `from_file` picks GraphSON for `.jsonl`.
    let back = GraphSource::from_file(&path).unwrap();
    assert_eq!(vertex_values(&*back.read(), "a"), expected());
    let (back, report) = GraphSONImport::new()
        .load_file(path.to_str().unwrap())
        .unwrap();
    assert_round_trip(&*back.read(), &report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "script")]
#[test]
fn graphson_script_functions_round_trip_exactly() {
    use std::sync::Arc;
    let dir = scratch("script");
    let path = dir.join("events.json");
    let path = path.to_str().unwrap().replace('\\', "/");
    let exported = graphersal::script::eval_value(
        Arc::new(source()),
        &format!(r#"g.export_graphson("{path}")"#),
    )
    .unwrap();
    assert_eq!(
        exported.into_string().unwrap(),
        format!("Exported GraphSON to {path}")
    );
    let target = Arc::new(GraphSource::empty());
    let message =
        graphersal::script::eval_value(target.clone(), &format!(r#"g.import_graphson("{path}")"#))
            .unwrap()
            .into_string()
            .unwrap();
    assert!(!message.contains("skipped"), "{message}");
    assert_eq!(vertex_values(&*target.read(), "a"), expected());
    let _ = std::fs::remove_dir_all(&dir);
}

/// One vertex property `p` holding `value` (GraphSON 3.0 line form).
fn one_value(value: &str) -> (Option<String>, ImportReport) {
    let line = format!(r#"{{"id":"v","label":"x","properties":{{"p":[{{"value":{value}}}]}}}}"#);
    let mut graph = TraversalGraph::new();
    let report = GraphSONImport::new()
        .import_reader(&mut graph, line.as_bytes())
        .unwrap();
    let v = graph.get_vertex_by_id("v").unwrap();
    let got = graph.vertex_properties(v).unwrap().get("p").map(exact);
    (got, report)
}

/// What `java.time`'s `toString()` writes (and so what TinkerGraph's GraphSON writer emits).
#[test]
fn graphson_import_reads_the_texts_java_time_writes() {
    let cases = [
        // Seconds left out when zero.
        (
            "gx:LocalDateTime",
            "2016-01-01T12:30",
            "localdatetime(2016-01-01T12:30:00)",
        ),
        ("gx:LocalTime", "12:30", "time(12:30:00)"),
        ("gx:OffsetTime", "10:15+01:00", "offsettime(10:15:00+01:00)"),
        (
            "gx:OffsetDateTime",
            "2007-12-03T10:15+01:00",
            "datetime(2007-12-03T10:15:00+01:00)",
        ),
        // Offsets with seconds (`ZoneOffset.ofHoursMinutesSeconds`).
        (
            "gx:OffsetTime",
            "10:15:30.000000001+03:06:09",
            "offsettime(10:15:30.000000001+03:06:09)",
        ),
        // Nine fraction digits, an instant in UTC.
        (
            "gx:Instant",
            "2016-12-14T16:39:19.349123456Z",
            "datetime(2016-12-14T16:39:19.349123456Z)",
        ),
        // `ZonedDateTime.toString()` of a UTC zone.
        (
            "gx:ZonedDateTime",
            "2016-12-23T12:12:24Z[UTC]",
            "datetime(2016-12-23T12:12:24Z[UTC])",
        ),
        // `Duration.toString()` signs each field.
        ("gx:Duration", "PT-1H-30M", "duration(-PT1H30M)"),
        ("gx:Duration", "PT-0.5S", "duration(-PT0.500S)"),
        (
            "gx:Duration",
            "PT-2562047788015215H-30M-8S",
            "duration(-PT2562047788015215H30M8S)",
        ),
        (
            "gx:Duration",
            "PT2562047788015215H30M7.999999999S",
            "duration(PT2562047788015215H30M7.999999999S)",
        ),
        // `Period.toString()` signs each field.
        ("gx:Period", "P1Y-2M3D", "duration(P10M3D)"),
        ("gx:Period", "P-1Y-2M", "duration(-P1Y2M)"),
        // The year edges.
        ("gx:LocalDate", "-9999-01-01", "date(-9999-01-01)"),
        (
            "gx:LocalDateTime",
            "+9999-12-31T23:59:59.999999999",
            "localdatetime(9999-12-31T23:59:59.999999999)",
        ),
    ];
    for (tag, text, want) in cases {
        let (got, report) = one_value(&format!(r#"{{"@type":"{tag}","@value":"{text}"}}"#));
        assert_eq!(got.as_deref(), Some(want), "{tag} {text}: {report}");
        assert!(report.notes.is_empty(), "{tag} {text}: {report}");
    }
}

/// The user decision of stage 4: a `gx:ZonedDateTime` whose stated offset is not its zone's
/// offset at that instant keeps the stated offset (the instant as written, never recomputed
/// from the name) and drops the name, with a `ZoneDropped` note; so does a zone id the bundled
/// database does not know.
#[test]
fn a_zoned_date_time_whose_offset_disagrees_with_its_zone_keeps_the_offset_and_drops_the_name() {
    for (text, want) in [
        // Berlin is at +01:00 in January.
        (
            "2024-01-01T10:00:00.000000001+05:00[Europe/Berlin]",
            "datetime(2024-01-01T10:00:00.000000001+05:00)",
        ),
        (
            "2016-12-23T12:12:24.000000036+02:00[GMT+02:00]",
            "datetime(2016-12-23T12:12:24.000000036+02:00)",
        ),
        (
            "2024-07-01T10:00:00+02:00[Nowhere/Atlantis]",
            "datetime(2024-07-01T10:00:00+02:00)",
        ),
    ] {
        let (got, report) = one_value(&format!(
            r#"{{"@type":"gx:ZonedDateTime","@value":"{text}"}}"#
        ));
        assert_eq!(got.as_deref(), Some(want), "{text}: {report}");
        assert!(!report.has_skips(), "{text}: {report}");
        assert!(
            report.notes.iter().any(|note| note.kind
                == ImportNoteKind::ZoneDropped {
                    type_tag: "gx:ZonedDateTime".into()
                }),
            "{text}: {report}"
        );
    }
}

/// Rule 7: GraphSON types a value only by its tag; a plain string that reads as a date stays a
/// string, and `ZonedDateTime` text under `gx:OffsetTime` or other wrong tags is malformed.
#[test]
fn only_the_tag_types_a_value() {
    let (got, report) = one_value(r#""2024-01-01T10:00:00.000000001+02:00[Europe/Berlin]""#);
    assert_eq!(
        got.as_deref(),
        Some(r#""2024-01-01T10:00:00.000000001+02:00[Europe/Berlin]""#)
    );
    assert!(report.notes.is_empty(), "{report}");
    for value in [
        r#"{"@type":"gx:OffsetTime","@value":"10:00:00"}"#,
        r#"{"@type":"gx:OffsetTime","@value":"10:00:00+02:00[Europe/Berlin]"}"#,
        r#"{"@type":"gx:LocalTime","@value":"10:00:00.0000000001"}"#,
        r#"{"@type":"gx:Duration","@value":"PT9223372036854775808S"}"#,
        r#"{"@type":"gx:Duration","@value":"-PT9223372036854775808.000000001S"}"#,
    ] {
        let (got, report) = one_value(value);
        assert_eq!(got, None, "{value}");
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.kind == ImportNoteKind::SkippedMalformedValue),
            "{value}: {report}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// GraphML
// ---------------------------------------------------------------------------------------------

fn graphml_schema() -> GraphSchema {
    let format = |kind: TemporalKind| match kind {
        TemporalKind::Date => "date",
        TemporalKind::LocalDateTime => "local-date-time",
        TemporalKind::DateTime => "date-time",
        TemporalKind::Time => "time",
        TemporalKind::OffsetTime => "offset-time",
        _ => "duration",
    };
    let properties: Vec<String> = VALUES
        .iter()
        .map(|(key, kind, _)| {
            format!(
                r#""{key}": {{"type": "string", "format": "{}"}}"#,
                format(*kind)
            )
        })
        .collect();
    GraphSchema::from_json(&format!(
        r#"{{"mode": "open", "vertices": {{"event": {{"schema": {{"type": "object", "properties": {{
            {},
            "nested": {{"type": "object", "properties": {{
                "at": {{"type": "string", "format": "date-time"}},
                "t": {{"type": "string", "format": "offset-time"}},
                "d": {{"type": "string", "format": "duration"}}}}}}
        }}}}}}}}, "edges": {{"next": {{"schema": {{"type": "object", "properties": {{
            "since": {{"type": "string", "format": "date-time"}},
            "took": {{"type": "string", "format": "duration"}}}}}}}}}}}}"#,
        properties.join(",\n")
    ))
    .unwrap()
}

#[test]
fn graphml_round_trips_every_kind_through_the_declared_formats() {
    let source = source();
    source.write().set_schema(graphml_schema()).unwrap();
    let mut out = Vec::new();
    source.export_graphml_writer(&mut out).unwrap();
    let xml = String::from_utf8(out).unwrap();
    // The canonical text of each value.
    for part in [
        ">2024-10-27T02:30:00.000000001+01:00[Europe/Berlin]<",
        ">00:00:00-18:00<",
        ">-PT2562047788015215H30M8S<",
        ">9999-12-31T23:59:59.999999999<",
    ] {
        assert!(xml.contains(part), "{part} missing in {xml}");
    }
    let mut target = GraphSource::empty();
    target.write().set_schema(graphml_schema()).unwrap();
    target
        .import_graphml_reader(std::io::Cursor::new(xml.into_bytes()))
        .unwrap();
    let lock = target.read();
    // `all` (an undeclared list) comes back as its JSON text; the key order follows the keys.
    let mut want: Vec<String> = expected()
        .into_iter()
        .filter(|line| !line.starts_with("all = "))
        .collect();
    let mut got: Vec<String> = vertex_values(&*lock, "a")
        .into_iter()
        .filter(|line| !line.starts_with("all = "))
        .collect();
    want.sort();
    got.sort();
    assert_eq!(got, want);
    assert_eq!(
        edge_values(&*lock),
        vec![
            format!("since = datetime({})", VALUES[8].2),
            format!("took = duration({})", VALUES[15].2),
        ]
    );
}

/// Without a declared format the GraphML text stays a string (rule 7), also when it reads as
/// an offset time or a zoned datetime.
#[test]
fn graphml_without_a_declared_format_keeps_the_text() {
    let xml = r#"<?xml version="1.0"?><graphml xmlns="http://graphml.graphdrawing.org/xmlns">
        <key id="t" for="node" attr.name="t" attr.type="string"/>
        <key id="z" for="node" attr.name="z" attr.type="string"/>
        <graph edgedefault="directed"><node id="v"><data key="t">10:00:00.000000001+02:00</data><data key="z">2024-03-31T03:30:00+02:00[Europe/Berlin]</data></node></graph></graphml>"#;
    let mut target = GraphSource::empty();
    target
        .import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()))
        .unwrap();
    assert_eq!(
        vertex_values(&*target.read(), "v"),
        vec![
            r#"t = "10:00:00.000000001+02:00""#.to_string(),
            r#"z = "2024-03-31T03:30:00+02:00[Europe/Berlin]""#.to_string(),
        ]
    );
}

/// A declared `offset-time` refuses a time without its offset (never read as UTC).
#[test]
fn graphml_refuses_text_that_is_not_of_the_declared_format() {
    let xml = r#"<?xml version="1.0"?><graphml xmlns="http://graphml.graphdrawing.org/xmlns">
        <key id="t" for="node" attr.name="ot_west" attr.type="string"/>
        <key id="l" for="node" attr.name="labelV" attr.type="string"/>
        <graph edgedefault="directed"><node id="v"><data key="l">event</data><data key="t">10:00:00</data></node></graph></graphml>"#;
    let mut target = GraphSource::empty();
    target.write().set_schema(graphml_schema()).unwrap();
    let err = target
        .import_graphml_reader(std::io::Cursor::new(xml.as_bytes().to_vec()))
        .unwrap_err();
    assert!(err.to_string().contains("offset"), "{err}");
    assert_eq!(target.read().vertex_count(), 0);
}

// ---------------------------------------------------------------------------------------------
// ChangeSet serde
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "serde")]
mod change_set_serde {
    use super::*;
    use graphersal::changes::{ChangeSet, Mutation};

    fn captured() -> ChangeSet {
        let mut graph = TraversalGraph::new();
        let ((), changes) = graph
            .dry_run(|g| {
                g.add_vertex(Some("a".into()), vec!["event".into()], properties())?;
                Ok::<_, GraphError>(())
            })
            .unwrap();
        changes
    }

    fn added(set: &ChangeSet) -> Vec<String> {
        set.mutations()
            .iter()
            .flat_map(|m| match m {
                Mutation::AddVertex { properties, .. } => properties
                    .iter()
                    .map(|(key, value)| format!("{key} = {}", exact(value)))
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn every_kind_round_trips_exactly_with_its_tag() {
        let set = captured();
        let text = serde_json::to_string(&set).unwrap();
        for part in [
            r#"{"@type":"gx:ZonedDateTime","@value":"2024-10-27T02:30:00.000000001+01:00[Europe/Berlin]"}"#,
            r#"{"@type":"gx:OffsetTime","@value":"23:59:59.999999999+18:00"}"#,
            r#"{"@type":"gx:LocalDateTime","@value":"9999-12-31T23:59:59.999999999"}"#,
            // The ChangeSet keeps the canonical text (our own format, read by our reader).
            r#"{"@type":"gx:Duration","@value":"-PT2562047788015215H30M8S"}"#,
            r#"{"@type":"gx:Period","@value":"P178956970Y7M2147483647D"}"#,
            r#"{"@type":"graphersal:Interval","@value":"P1M-2DT0.000000003S"}"#,
        ] {
            assert!(text.contains(part), "{part} missing in {text}");
        }
        let back: ChangeSet = serde_json::from_str(&text).unwrap();
        assert_eq!(added(&back), expected());
        // Replayed into an empty graph, the values are identical.
        let mut replay = TraversalGraph::new();
        replay.apply_change_set(&back).unwrap();
        assert_eq!(vertex_values(&replay, "a"), expected());
    }

    #[test]
    fn a_zoned_tag_without_a_zone_name_and_an_offset_time_without_offset_are_refused() {
        for bad in [
            r#"{"@type":"gx:ZonedDateTime","@value":"2024-01-01T10:00:00+01:00"}"#,
            r#"{"@type":"gx:ZonedDateTime","@value":"2024-01-01T10:00:00+05:00[Europe/Berlin]"}"#,
            r#"{"@type":"gx:OffsetTime","@value":"10:00:00"}"#,
            r#"{"@type":"gx:Duration","@value":"PT9223372036854775808S"}"#,
        ] {
            let text = format!(
                r#"{{"commit_seq":1,"mutations":[{{"op":"set_property","element":{{"kind":"vertex","id":"1"}},"key":"k","after":{bad}}}]}}"#
            );
            assert!(
                serde_json::from_str::<ChangeSet>(&text).is_err(),
                "{bad} must be refused"
            );
        }
    }
}
