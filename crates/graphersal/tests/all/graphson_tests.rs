//! GraphSON 3.0 import and export (`io::graphson`): the TinkerPop sample graphs, round trips,
//! every type mapping, skipped values in the report, malformed input, units and the DSL.
#![cfg(feature = "io")]

use std::collections::BTreeMap;
use std::path::PathBuf;

use graphersal::io::{GraphExport, GraphImport, GraphSONImport, ImportNoteKind, ImportReport};
use graphersal::prelude::*;
use graphersal::prelude::{Graph, PropertyMap};

fn data(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

/// The modern graph in GraphSON 3.0 line form, hand-written in the shape of TinkerPop's
/// `gremlin-test/.../io/graphson/tinkerpop-modern-v3.json` (Apache-2.0): vertex ids and edge
/// ids as `g:Int32`, vertex-property ids as `g:Int64`, weights as `g:Double`.
fn modern_v3() -> Vec<u8> {
    std::fs::read(data("tests/data/tinkerpop-modern-v3.json")).expect("modern v3 fixture")
}

fn crew_v3() -> Vec<u8> {
    std::fs::read(data("tests/tinkerpop/data/tinkerpop-crew.json")).expect("crew fixture")
}

fn load(text: &[u8]) -> (Graph, ImportReport) {
    GraphSONImport::new()
        .load(std::io::Cursor::new(text))
        .expect("import")
}

fn load_str(text: &str) -> (Graph, ImportReport) {
    load(text.as_bytes())
}

fn export(graph: &Graph) -> String {
    let mut out = Vec::new();
    graph.export_graphson_writer(&mut out).expect("export");
    String::from_utf8(out).expect("UTF-8")
}

type VertexSnapshot = BTreeMap<String, (Vec<String>, PropertyMap)>;
type EdgeSnapshot = BTreeMap<String, (Option<String>, String, String, PropertyMap)>;

/// Every vertex (id -> labels, properties) and every edge (id -> label, from, to, properties).
fn snapshot(graph: &Graph) -> (VertexSnapshot, EdgeSnapshot) {
    let g = graph.read();
    let mut vertices = BTreeMap::new();
    for v in g.vertices().unwrap() {
        let id = g.vertex_id(v).unwrap().expect("id").to_string();
        let labels = g
            .vertex_labels(v)
            .unwrap()
            .iter()
            .map(|l| l.to_string())
            .collect();
        let props = g.vertex_properties(v).unwrap().cloned().unwrap_or_default();
        vertices.insert(id, (labels, props));
    }
    let mut edges = BTreeMap::new();
    for e in g.edges().unwrap() {
        let id = g.edge_id(e).unwrap().expect("edge id").to_string();
        let (from, to) = g.edge_endpoints(e).unwrap().expect("endpoints");
        edges.insert(
            id,
            (
                g.edge_label(e).unwrap().map(str::to_string),
                g.vertex_id(from).unwrap().expect("from").to_string(),
                g.vertex_id(to).unwrap().expect("to").to_string(),
                g.edge_properties(e).unwrap().cloned().unwrap_or_default(),
            ),
        );
    }
    (vertices, edges)
}

fn vertex_props(graph: &Graph, id: &str) -> PropertyMap {
    let g = graph.read();
    let v = g.get_vertex_by_id(id).unwrap().expect("vertex");
    g.vertex_properties(v).unwrap().cloned().unwrap_or_default()
}

fn note(report: &ImportReport, pred: impl Fn(&ImportNoteKind) -> bool) -> Option<usize> {
    report.notes.iter().find(|n| pred(&n.kind)).map(|n| n.count)
}

fn obj(pairs: &[(&str, ElementProperty)]) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (k, v) in pairs {
        map.insert(*k, v.clone());
    }
    ElementProperty::Object(Box::new(map))
}

// ---------------------------------------------------------------------------------------------
// The TinkerPop sample graphs
// ---------------------------------------------------------------------------------------------

#[test]
fn modern_v3_matches_the_vendored_graphml_modern() {
    let (from_graphson, report) = load(&modern_v3());
    assert_eq!((report.vertices, report.edges), (6, 6));
    assert!(report.notes.is_empty(), "{report}");
    let graphml = std::fs::read(data("tests/tinkerpop/data/tinkerpop-modern.xml")).unwrap();
    let from_graphml = GraphSource::from_graphml_reader(std::io::Cursor::new(graphml)).unwrap();
    assert_eq!(snapshot(&from_graphson), snapshot(&from_graphml));
}

#[test]
fn modern_v3_matches_the_built_in_modern_graph_up_to_edge_ids() {
    let (from_graphson, _) = load(&modern_v3());
    let built_in = GraphSource::tinkerpop_modern();
    let (v1, e1) = snapshot(&from_graphson);
    let (v2, e2) = snapshot(&built_in);
    assert_eq!(v1, v2);
    let by_shape = |edges: EdgeSnapshot| {
        let mut list: Vec<_> = edges
            .into_values()
            .map(|(l, f, t, p)| ((l, f, t), p))
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    };
    assert_eq!(by_shape(e1), by_shape(e2));
    // And it answers queries like the built-in one.
    let names = from_graphson
        .read()
        .traversal()
        .v("1")
        .out("knows")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names.len(), 2);
}

#[test]
fn crew_multi_and_meta_properties_import_as_arrays_and_meta() {
    let (graph, report) = load(&crew_v3());
    assert_eq!((report.vertices, report.edges), (6, 14));
    assert!(!report.has_skips(), "{report}");
    assert!(note(&report, |k| *k == ImportNoteKind::MultiValuedToArray).is_some());
    assert!(note(&report, |k| *k == ImportNoteKind::MetaPropertiesToMeta).is_some());
    let marko = vertex_props(&graph, "1");
    assert_eq!(
        marko.get("location"),
        Some(&ElementProperty::Array(vec![
            "san diego".into(),
            "santa cruz".into(),
            "brussels".into(),
            "santa fe".into(),
        ]))
    );
    let meta = marko.get("_meta").expect("_meta");
    let ElementProperty::Object(meta) = meta else {
        panic!("_meta is an object: {meta:?}");
    };
    assert_eq!(
        meta.get("location"),
        Some(&ElementProperty::Array(vec![
            obj(&[("startTime", 1997.into()), ("endTime", 2001.into())]),
            obj(&[("startTime", 2001.into()), ("endTime", 2004.into())]),
            obj(&[("startTime", 2004.into()), ("endTime", 2005.into())]),
            obj(&[("startTime", 2005.into())]),
        ]))
    );
    // A single-valued property keeps its scalar value and gets no `_meta` entry.
    assert_eq!(marko.get("name"), Some(&ElementProperty::from("marko")));
    assert!(meta.get("name").is_none());
}

#[test]
fn crew_export_restores_multi_and_meta_properties() {
    let (graph, _) = load(&crew_v3());
    let text = export(&graph);
    let marko = text
        .lines()
        .find(|l| l.starts_with(r#"{"id":"1","#))
        .expect("marko's line");
    assert!(!marko.contains("\"_meta\""), "{marko}");
    assert_eq!(marko.matches(r#""startTime""#).count(), 4, "{marko}");
    assert!(
        marko.contains(r#""value":"san diego","properties":{"startTime":{"@type":"g:Int64","@value":1997},"endTime":{"@type":"g:Int64","@value":2001}}"#),
        "{marko}"
    );
    let (again, report) = load_str(&text);
    assert!(!report.has_skips(), "{report}");
    assert_eq!(snapshot(&again), snapshot(&graph));
}

#[test]
fn sink_self_loop_is_imported_once() {
    let sink = std::fs::read(data("tests/tinkerpop/data/tinkerpop-sink.json")).unwrap();
    let (graph, report) = load(&sink);
    assert!(report.notes.is_empty(), "{report}");
    let (vertices, edges) = snapshot(&graph);
    assert_eq!(vertices.len(), 3);
    assert_eq!(
        edges.get("2003").map(|e| (e.1.as_str(), e.2.as_str())),
        Some(("2000", "2000"))
    );
    assert_eq!(edges.len(), report.edges);
}

// ---------------------------------------------------------------------------------------------
// Round trips
// ---------------------------------------------------------------------------------------------

fn every_type_graph() -> Graph {
    let graph = GraphSource::empty();
    {
        let mut g = graph.write();
        let mut props = PropertyMap::default();
        props.insert("i", (-42i64).into());
        props.insert("big", i64::MAX.into());
        props.insert("f", 2.5f64.into());
        props.insert("whole", 3.0f64.into());
        props.insert("nan", f64::NAN.into());
        props.insert("inf", f64::INFINITY.into());
        props.insert("ninf", f64::NEG_INFINITY.into());
        props.insert("s", "a \"quoted\"\nline \u{1F600}".into());
        props.insert("uuid_text", "41d2e28a-20a4-4ab0-b379-d810dede3786".into());
        props.insert(
            "u",
            ElementProperty::Uuid(0x41d2_e28a_20a4_4ab0_b379_d810_dede_3786),
        );
        props.insert("b", true.into());
        props.insert("n", ElementProperty::Null);
        props.insert(
            "list",
            ElementProperty::Array(vec![1i64.into(), "x".into(), ElementProperty::Null]),
        );
        props.insert("one", ElementProperty::Array(vec!["only".into()]));
        props.insert("empty_list", ElementProperty::Array(vec![]));
        props.insert(
            "nested",
            obj(&[
                (
                    "a",
                    ElementProperty::Array(vec![obj(&[("deep", 1.5f64.into())])]),
                ),
                ("@type", "not a tag".into()),
                ("empty", obj(&[])),
            ]),
        );
        let a = g
            .add_vertex(Some("a".into()), vec!["A".into(), "B".into()], props)
            .unwrap();
        let mut meta_props = PropertyMap::default();
        meta_props.insert("name", "b".into());
        meta_props.insert("_meta", obj(&[("name", obj(&[("since", 2001i64.into())]))]));
        let b = g
            .add_vertex(Some("b".into()), vec!["B".into()], meta_props)
            .unwrap();
        // A `_meta` that does not have the import shape stays a plain property.
        let mut plain_meta = PropertyMap::default();
        plain_meta.insert("_meta", "just text".into());
        let c = g
            .add_vertex(Some("c".into()), vec!["C".into()], plain_meta)
            .unwrap();
        let mut ep = PropertyMap::default();
        ep.insert("w", 0.5f64.into());
        ep.insert("tags", ElementProperty::Array(vec!["t".into()]));
        g.add_edge(Some("e1".into()), Some("knows".into()), a, b, ep)
            .unwrap();
        g.add_edge(
            Some("e2".into()),
            Some("self".into()),
            c,
            c,
            PropertyMap::default(),
        )
        .unwrap();
        g.add_edge(
            Some("e3".into()),
            Some("knows".into()),
            b,
            a,
            PropertyMap::default(),
        )
        .unwrap();
    }
    graph
}

#[test]
fn round_trip_is_lossless_for_every_type() {
    let graph = every_type_graph();
    let text = export(&graph);
    let (again, report) = load_str(&text);
    assert!(!report.has_skips(), "{report}");
    assert_eq!(snapshot(&again), snapshot(&graph), "{text}");
    // Exporting the re-import gives the same text.
    assert_eq!(export(&again), text);
}

#[test]
fn export_writes_graphson_3_typed_values() {
    let text = export(&every_type_graph());
    let a = text.lines().next().expect("first line");
    for expected in [
        r#"{"id":"a","label":"A::B","#,
        r#""i":[{"id":{"@type":"g:Int64","@value":0},"value":{"@type":"g:Int64","@value":-42}}]"#,
        r#""value":{"@type":"g:Double","@value":2.5}"#,
        r#""value":{"@type":"g:Double","@value":3.0}"#,
        r#""value":{"@type":"g:Double","@value":"NaN"}"#,
        r#""value":{"@type":"g:Double","@value":"Infinity"}"#,
        r#""value":{"@type":"g:Double","@value":"-Infinity"}"#,
        r#""value":{"@type":"g:UUID","@value":"41d2e28a-20a4-4ab0-b379-d810dede3786"}"#,
        r#""value":"41d2e28a-20a4-4ab0-b379-d810dede3786""#,
        r#""value":{"@type":"g:List","@value":[{"@type":"g:Int64","@value":1},"x",null]}"#,
        r#""value":{"@type":"g:Map","@value":["a",{"@type":"g:List""#,
        r#""value":null"#,
        r#""outE":{"knows":[{"id":"e1","inV":"b","properties":{"w":{"@type":"g:Double","@value":0.5},"tags":{"@type":"g:List","@value":["t"]}}}]}"#,
        r#""inE":{"knows":[{"id":"e3","outV":"b"}]}"#,
    ] {
        assert!(a.contains(expected), "missing {expected} in\n{a}");
    }
    // Every line is one JSON object; `inE` precedes `outE` as in TinkerPop's writer.
    assert_eq!(text.lines().count(), 3);
    assert!(a.find("\"inE\"").unwrap() < a.find("\"outE\"").unwrap());
    // `_meta` with the import shape becomes meta-properties, other `_meta`s stay properties.
    let b = text.lines().nth(1).unwrap();
    assert!(
        b.contains(r#""value":"b","properties":{"since":{"@type":"g:Int64","@value":2001}}"#),
        "{b}"
    );
    assert!(!b.contains("_meta"), "{b}");
    let c = text.lines().nth(2).unwrap();
    assert!(c.contains(r#""_meta":[{"#), "{c}");
    // Vertex-property ids are unique in the file.
    assert!(text.contains(r#""id":{"@type":"g:Int64","@value":0}"#));
}

#[test]
fn unlabeled_elements_get_tinkerpops_default_labels() {
    let graph = GraphSource::empty();
    {
        let mut g = graph.write();
        let a = g
            .add_vertex(Some("a".into()), vec![], PropertyMap::default())
            .unwrap();
        g.add_edge(Some("e".into()), None, a, a, PropertyMap::default())
            .unwrap();
    }
    let text = export(&graph);
    assert!(
        text.starts_with(r#"{"id":"a","label":"vertex","inE":{"edge":"#),
        "{text}"
    );
    let (again, _) = load_str(&text);
    let (vertices, edges) = snapshot(&again);
    assert_eq!(vertices["a"].0, vec!["vertex".to_string()]);
    assert_eq!(edges["e"].0.as_deref(), Some("edge"));
}

#[test]
fn file_round_trip_through_graph_source() {
    let dir = std::env::temp_dir().join(format!("graphersal-graphson-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let graph = every_type_graph();
    for name in ["g.json", "g.JSONL", "g.graphson"] {
        let path = dir.join(name);
        graph.export_graphson(path.to_str().unwrap()).unwrap();
        let loaded = GraphSource::from_file(&path).unwrap();
        assert_eq!(snapshot(&loaded), snapshot(&graph), "{name}");
        let loaded = GraphSource::from_graphson(&path).unwrap();
        assert_eq!(snapshot(&loaded), snapshot(&graph), "{name}");
    }
    assert!(GraphSource::is_graphson_path("x.Json"));
    assert!(!GraphSource::is_graphson_path("x.xml"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traversal_source_exports_graphson() {
    let graph = GraphSource::tinkerpop_modern();
    let mut out = Vec::new();
    graph
        .read()
        .traversal()
        .export_graphson_writer(&mut out)
        .unwrap();
    let (again, _) = load(&out);
    assert_eq!(snapshot(&again), snapshot(&graph));
}

// ---------------------------------------------------------------------------------------------
// Type mappings
// ---------------------------------------------------------------------------------------------

/// Imports one vertex whose property `p` has the GraphSON value `value`.
fn one_value(value: &str) -> (Option<ElementProperty>, ImportReport) {
    let line = format!(r#"{{"id":"v","label":"x","properties":{{"p":[{{"value":{value}}}]}}}}"#);
    let (graph, report) = load_str(&line);
    (vertex_props(&graph, "v").get("p").cloned(), report)
}

#[test]
fn every_type_mapping() {
    let uuid = ElementProperty::Uuid(0x41d2_e28a_20a4_4ab0_b379_d810_dede_3786);
    let cases: Vec<(&str, ElementProperty)> = vec![
        (r#""text""#, "text".into()),
        ("true", true.into()),
        ("null", ElementProperty::Null),
        ("7", 7i64.into()),
        ("7.5", 7.5f64.into()),
        (r#"{"@type":"g:Int32","@value":7}"#, 7i64.into()),
        (
            r#"{"@type":"g:Int64","@value":-9223372036854775808}"#,
            i64::MIN.into(),
        ),
        (r#"{"@type":"gx:Int16","@value":3}"#, 3i64.into()),
        (r#"{"@type":"gx:Byte","@value":1}"#, 1i64.into()),
        (r#"{"@type":"gx:BigInteger","@value":12}"#, 12i64.into()),
        (r#"{"@type":"g:Double","@value":0.25}"#, 0.25f64.into()),
        (r#"{"@type":"g:Double","@value":2}"#, 2.0f64.into()),
        (r#"{"@type":"g:Float","@value":0.1}"#, 0.1f64.into()),
        (
            r#"{"@type":"g:Double","@value":"Infinity"}"#,
            f64::INFINITY.into(),
        ),
        (
            r#"{"@type":"g:Double","@value":"-Infinity"}"#,
            f64::NEG_INFINITY.into(),
        ),
        (r#"{"@value":0.5,"@type":"g:Double"}"#, 0.5f64.into()),
        (
            r#"{"@type":"g:UUID","@value":"41d2e28a-20a4-4ab0-b379-d810dede3786"}"#,
            uuid.clone(),
        ),
        (
            r#"{"@type":"g:UUID","@value":"41D2E28A-20A4-4AB0-B379-D810DEDE3786"}"#,
            uuid,
        ),
        (r#"{"@type":"gx:Char","@value":"c"}"#, "c".into()),
        (
            r#"{"@type":"g:List","@value":[{"@type":"g:Int32","@value":1},"a"]}"#,
            ElementProperty::Array(vec![1i64.into(), "a".into()]),
        ),
        (
            r#"{"@type":"g:Set","@value":["a","b"]}"#,
            ElementProperty::Array(vec!["a".into(), "b".into()]),
        ),
        (
            r#"{"@type":"g:Map","@value":["k",{"@type":"g:Int32","@value":1},"m",{"@type":"g:Map","@value":[]}]}"#,
            obj(&[("k", 1i64.into()), ("m", obj(&[]))]),
        ),
        (
            r#"[1,{"a":[true]}]"#,
            ElementProperty::Array(vec![
                1i64.into(),
                obj(&[("a", ElementProperty::Array(vec![true.into()]))]),
            ]),
        ),
        (
            r#"{"plain":{"@type":"g:Int32","@value":4}}"#,
            obj(&[("plain", 4i64.into())]),
        ),
    ];
    for (text, expected) in cases {
        let (value, report) = one_value(text);
        assert_eq!(value, Some(expected), "{text}");
        assert!(!report.has_skips(), "{text}: {report}");
    }
    let (nan, _) = one_value(r#"{"@type":"g:Double","@value":"NaN"}"#);
    assert!(matches!(nan, Some(ElementProperty::Number(n)) if n.as_f64().is_some_and(f64::is_nan)));
}

#[test]
fn date_types_without_a_twin_are_skipped_and_reported_never_converted() {
    // The `java.time` types Graphersal has no value for are skipped and reported, never
    // converted into something else (a year into an integer, an offset into a string; an
    // offset time is a value since temporal v2).
    for (tag, payload) in [
        ("gx:Year", r#""2016""#),
        ("gx:YearMonth", r#""2016-06""#),
        ("gx:MonthDay", r#""--12-03""#),
        ("gx:ZoneOffset", r#""+01:00""#),
        ("g:Calendar", "1481750076295"),
        ("g:TimeZone", r#""Europe/Paris""#),
    ] {
        let (value, report) = one_value(&format!(r#"{{"@type":"{tag}","@value":{payload}}}"#));
        assert_eq!(value, None, "{tag}");
        assert_eq!(
            note(&report, |k| *k
                == ImportNoteKind::SkippedDateTime {
                    type_tag: tag.to_string()
                }),
            Some(1),
            "{tag}: {report}"
        );
        assert!(report.has_skips());
        let sample = &report.notes[0].samples[0];
        assert!(
            sample.contains(r#"line 1: vertex "v", property "p""#),
            "{sample}"
        );
        assert!(report.to_string().contains(tag), "{report}");
    }
    // The date/time types Graphersal holds are converted to its own values, nothing skipped.
    let instant = |text: &str| -> ElementProperty {
        ElementProperty::from(text.parse::<graphersal::temporal::DateTime>().unwrap())
    };
    for (tag, payload, expected) in [
        (
            "g:Date",
            "1481750076295",
            instant("2016-12-14T21:14:36.295Z"),
        ),
        (
            "g:Timestamp",
            "1481750076295",
            instant("2016-12-14T21:14:36.295Z"),
        ),
        (
            "gx:OffsetDateTime",
            r#""2016-12-14T16:39:19.349Z""#,
            instant("2016-12-14T16:39:19.349Z"),
        ),
        (
            "g:OffsetDateTime",
            r#""2016-12-14T16:39:19.349Z""#,
            instant("2016-12-14T16:39:19.349Z"),
        ),
        (
            "gx:Instant",
            r#""2016-12-14T16:39:19.349Z""#,
            instant("2016-12-14T16:39:19.349Z"),
        ),
        (
            "gx:LocalDate",
            r#""2016-12-14""#,
            ElementProperty::from("2016-12-14".parse::<graphersal::temporal::Date>().unwrap()),
        ),
        (
            "gx:Duration",
            r#""PT120H""#,
            ElementProperty::from("PT120H".parse::<graphersal::temporal::Duration>().unwrap()),
        ),
    ] {
        let (value, report) = one_value(&format!(r#"{{"@type":"{tag}","@value":{payload}}}"#));
        assert_eq!(value, Some(expected), "{tag}: {report}");
        assert!(!report.has_skips(), "{tag}: {report}");
    }
    // A date/time value we cannot hold inside a container skips the whole value.
    let (value, report) =
        one_value(r#"{"@type":"g:List","@value":["a",{"@type":"gx:Year","@value":"2016"}]}"#);
    assert_eq!(value, None);
    assert_eq!(report.skipped(), 1);
    // One we hold is converted inside it.
    let (value, report) =
        one_value(r#"{"@type":"g:List","@value":["a",{"@type":"g:Date","@value":1}]}"#);
    assert_eq!(
        value,
        Some(ElementProperty::Array(vec![
            "a".into(),
            instant("1970-01-01T00:00:00.001Z")
        ]))
    );
    assert_eq!(report.skipped(), 0);
}

#[test]
fn unsupported_and_malformed_values_are_skipped() {
    for (text, kind) in [
        (
            r#"{"@type":"gx:BigDecimal","@value":1.5}"#,
            ImportNoteKind::SkippedUnsupportedType {
                type_tag: "gx:BigDecimal".into(),
            },
        ),
        (
            r#"{"@type":"g:Class","@value":"java.io.File"}"#,
            ImportNoteKind::SkippedUnsupportedType {
                type_tag: "g:Class".into(),
            },
        ),
        (
            r#"{"@type":"janusgraph:Geoshape","@value":{"type":"Point"}}"#,
            ImportNoteKind::SkippedUnsupportedType {
                type_tag: "janusgraph:Geoshape".into(),
            },
        ),
        (
            r#"{"@type":"g:Vertex","@value":{"id":1,"label":"x"}}"#,
            ImportNoteKind::SkippedUnsupportedType {
                type_tag: "g:Vertex".into(),
            },
        ),
        (
            r#"{"@type":"g:Int32","@value":"7"}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
        (
            r#"{"@type":"g:Int32","@value":7.5}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
        (
            r#"{"@type":"g:Double","@value":"lots"}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
        (
            r#"{"@type":"g:UUID","@value":"nope"}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
        (
            r#"{"@type":"g:Map","@value":["odd"]}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
        (
            r#"{"@type":"g:Map","@value":[{"@type":"g:Int32","@value":1},"x"]}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
        (
            r#"{"@type":"g:List","@value":{}}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
        (
            r#"{"@type":"g:Int32","@value":1,"extra":2}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
        (
            r#"{"@type":5,"@value":1}"#,
            ImportNoteKind::SkippedMalformedValue,
        ),
    ] {
        let (value, report) = one_value(text);
        assert_eq!(value, None, "{text}");
        assert_eq!(note(&report, |k| *k == kind), Some(1), "{text}: {report}");
    }
}

#[test]
fn values_nested_beyond_the_depth_limit_are_rejected() {
    // 200 plain nested arrays: within the JSON parser's bound, beyond the value depth.
    let deep = format!("{}1{}", "[".repeat(200), "]".repeat(200));
    let (value, report) = one_value(&deep);
    assert_eq!(value, None);
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::SkippedMalformedValue),
        Some(1)
    );
    // Far deeper: the parser itself refuses the line (no stack exhaustion).
    let deeper = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
    let line = format!(r#"{{"id":"v","properties":{{"p":[{{"value":{deeper}}}]}}}}"#);
    let err = GraphSONImport::new()
        .load(std::io::Cursor::new(line))
        .expect_err("must fail");
    assert!(
        matches!(err, IOError::GraphSONParse { line: 1, .. }),
        "{err}"
    );
}

// ---------------------------------------------------------------------------------------------
// Structure
// ---------------------------------------------------------------------------------------------

#[test]
fn ids_labels_and_unknown_keys() {
    let text = concat!(
        r#"{"id":{"@type":"g:Int64","@value":10},"label":"A::B::","type":"vertex","_partition":"p"}"#,
        "\n",
        r#"{"id":{"@type":"g:UUID","@value":"41D2E28A-20A4-4AB0-B379-D810DEDE3786"},"label":"u"}"#,
        "\n",
        r#"{"id":{"@type":"janusgraph:RelationIdentifier","@value":{"relationId":"4r6-39s"}},"label":"j"}"#,
        "\n",
        r#"{"label":"no-id"}"#,
        "\n",
        r#"{"id":"10","label":"dup"}"#,
        "\n",
        r#"{"id":2.5}"#,
        "\n",
    );
    let (graph, report) = load_str(text);
    let (vertices, _) = snapshot(&graph);
    assert_eq!(vertices["10"].0, vec!["A".to_string(), "B".to_string()]);
    assert!(vertices.contains_key("41d2e28a-20a4-4ab0-b379-d810dede3786"));
    assert!(vertices.contains_key("4r6-39s"));
    assert!(vertices.contains_key("2.5"));
    assert!(vertices["2.5"].0.is_empty());
    assert_eq!(vertices.len(), 5);
    assert_eq!(note(&report, |k| *k == ImportNoteKind::LabelSplit), Some(1));
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::VertexWithoutId),
        Some(1)
    );
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::SkippedDuplicateVertex),
        Some(1)
    );
    assert_eq!(
        note(&report, |k| *k
            == ImportNoteKind::IgnoredKey { key: "type".into() }),
        Some(1)
    );
    assert_eq!(
        note(&report, |k| *k
            == ImportNoteKind::IgnoredKey {
                key: "_partition".into()
            }),
        Some(1)
    );
}

#[test]
fn edges_from_in_only_out_only_and_without_ids() {
    // Written with Direction.IN only: every edge comes from inE.
    let in_only = concat!(
        r#"{"id":"a","label":"x"}"#,
        "\n",
        r#"{"id":"b","label":"x","inE":{"l":[{"id":"e1","outV":"a","properties":{"w":1}}]}}"#,
    );
    let (graph, report) = load_str(in_only);
    assert_eq!(report.edges, 1);
    assert_eq!(snapshot(&graph).1["e1"].1, "a");
    // Written with Direction.OUT only.
    let out_only = concat!(
        r#"{"id":"a","outE":{"l":[{"id":"e1","inV":"b"}]}}"#,
        "\n",
        r#"{"id":"b"}"#,
    );
    let (graph, _) = load_str(out_only);
    assert_eq!(snapshot(&graph).1["e1"].2, "b");
    // Without ids: the outE copy is taken, the inE copy is recognized as such; an inE-only edge
    // (its tail has no outE) is added.
    let no_ids = concat!(
        r#"{"id":"a","outE":{"l":[{"inV":"b"}]},"inE":{"m":[{"outV":"c"}]}}"#,
        "\n",
        r#"{"id":"b","inE":{"l":[{"outV":"a"}]}}"#,
        "\n",
        r#"{"id":"c"}"#,
    );
    let (graph, report) = load_str(no_ids);
    assert_eq!(report.edges, 2);
    assert_eq!(graph.read().edge_count(), 2);
}

#[test]
fn dangling_duplicate_and_disagreeing_edges_are_reported() {
    let text = concat!(
        r#"{"id":"a","outE":{"l":[{"id":"e1","inV":"zz"},{"id":"e2","inV":"b"},{"id":"e2","inV":"a"}]}}"#,
        "\n",
        r#"{"id":"b","inE":{"other":[{"id":"e2","outV":"a"}]},"outE":{"l":[{"inV":"a"}],"bad":5}}"#,
    );
    let (graph, report) = load_str(text);
    assert_eq!(report.edges, 2);
    assert_eq!(graph.read().edge_count(), 2);
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::SkippedDanglingEdge),
        Some(1)
    );
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::SkippedDuplicateEdge),
        Some(1)
    );
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::EdgeCopiesDisagree),
        Some(1)
    );
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::SkippedMalformedEntry),
        Some(1)
    );
    let sample = &report
        .notes
        .iter()
        .find(|n| n.kind == ImportNoteKind::SkippedDanglingEdge)
        .unwrap()
        .samples[0];
    assert_eq!(sample, r#"line 1: edge "e1" from "a" to "zz""#);
}

#[test]
fn edges_may_point_at_vertices_already_in_the_target_graph() {
    let mut graph = GraphSource::tinkerpop_modern();
    let report = graph
        .import_graphson_reader(
            r#"{"id":"new","outE":{"likes":[{"id":"x","inV":"1"}]}}"#.as_bytes(),
        )
        .unwrap();
    assert_eq!(report.edges, 1);
    assert_eq!(graph.read().edge_count(), 7);
}

#[test]
fn malformed_property_entries_and_reserved_meta() {
    let text = r#"{"id":"v","properties":{"a":[5,{"id":1},{"value":"ok"}],"b":"scalar","c":{"value":"single"},"d":[],"_meta":[{"value":"mine"}],"m":[{"value":1,"properties":{"since":{"@type":"gx:Year","@value":"2016"},"at":{"@type":"g:Date","@value":1},"by":"x"}}]}}"#;
    let (graph, report) = load_str(text);
    let props = vertex_props(&graph, "v");
    assert_eq!(props.get("a"), Some(&"ok".into()));
    assert_eq!(props.get("b"), None);
    assert_eq!(props.get("c"), Some(&"single".into()));
    assert_eq!(props.get("d"), None);
    assert_eq!(props.get("m"), Some(&1i64.into()));
    assert_eq!(
        props.get("_meta"),
        Some(&obj(&[(
            "m",
            obj(&[
                // A date/time meta-property Graphersal holds is converted; `since` (a year,
                // which it cannot hold) is skipped below.
                (
                    "at",
                    ElementProperty::from(
                        "1970-01-01T00:00:00.001Z"
                            .parse::<graphersal::temporal::DateTime>()
                            .unwrap()
                    )
                ),
                ("by", "x".into())
            ])
        )]))
    );
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::SkippedMalformedEntry),
        Some(3)
    );
    assert_eq!(
        note(&report, |k| *k == ImportNoteKind::SkippedReservedMeta),
        Some(1)
    );
    assert!(
        note(&report, |k| *k
            == ImportNoteKind::SkippedDateTime {
                type_tag: "gx:Year".into()
            })
        .is_some()
    );
    // Without meta-properties a `_meta` of the file is an ordinary property.
    let (graph, report) = load_str(r#"{"id":"v","properties":{"_meta":[{"value":"mine"}]}}"#);
    assert_eq!(vertex_props(&graph, "v").get("_meta"), Some(&"mine".into()));
    assert!(report.notes.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------------------------

#[test]
fn wrapped_form_compact_and_pretty() {
    let lines = String::from_utf8(modern_v3()).unwrap();
    let vertices: Vec<&str> = lines.lines().collect();
    let compact = format!(r#"{{"vertices":[{}],"extra":1}}"#, vertices.join(","));
    let (graph, report) = load_str(&compact);
    assert_eq!((report.vertices, report.edges), (6, 6));
    assert_eq!(
        note(&report, |k| *k
            == ImportNoteKind::IgnoredKey {
                key: "extra".into()
            }),
        Some(1)
    );
    let (reference, _) = load(&modern_v3());
    assert_eq!(snapshot(&graph), snapshot(&reference));
    // TinkerPop's wrapped writer: one vertex per line inside the wrapper.
    let pretty = format!("{{\"vertices\":[\n{}\n]}}\n", vertices.join(",\n"));
    let (graph, _) = load_str(&pretty);
    assert_eq!(snapshot(&graph), snapshot(&reference));
    // Wrapped form samples name the array position.
    let (_, report) = load_str(r#"{"vertices":[{"id":"a"},{"id":"a"}]}"#);
    assert_eq!(report.notes[0].samples[0], r#"vertices[1]: vertex "a""#);
}

#[test]
fn bom_crlf_and_blank_lines_are_accepted() {
    let lines = String::from_utf8(modern_v3()).unwrap();
    let text = format!("\u{feff}\r\n{}\r\n\r\n", lines.replace('\n', "\r\n\n"));
    let (graph, _) = load_str(&text);
    let (reference, _) = load(&modern_v3());
    assert_eq!(snapshot(&graph), snapshot(&reference));
    let (empty, report) = load_str("");
    assert_eq!(empty.read().vertex_count(), 0);
    assert_eq!(report.vertices, 0);
}

/// A predicate on the expected import error.
type ErrorCheck = fn(&IOError) -> bool;

#[test]
fn malformed_input_fails_with_the_line() {
    let cases: Vec<(String, ErrorCheck)> = vec![
        ("{\"id\":\"a\"}\n{\"id\": oops}\n".to_string(), |e| {
            matches!(e, IOError::GraphSONParse { line: 2, .. })
        }),
        ("{\"id\":\"a\"}\n[1,2]\n".to_string(), |e| {
            matches!(e, IOError::GraphSONParse { line: 2, .. })
        }),
        ("{\"id\":\"a\"\n".to_string(), |e| {
            matches!(e, IOError::GraphSONParse { line: 1, .. })
        }),
        (
            "{\"id\":\"a\"}\n{\"id\":99999999999999999999}\n".to_string(),
            |e| matches!(e, IOError::GraphSONParse { line: 2, .. }),
        ),
        ("{\"id\":\"a\"}\n\u{0}\n".to_string(), |e| {
            matches!(e, IOError::GraphSONParse { line: 2, .. })
        }),
        (
            r#"{"@class":"java.util.HashMap","id":1}"#.to_string(),
            |e| matches!(e, IOError::UnsupportedGraphSON { line: 1, .. }),
        ),
        (r#"{"mode":"EXTENDED","vertices":[]}"#.to_string(), |e| {
            matches!(e, IOError::UnsupportedGraphSON { .. })
        }),
        (r#"{"_id":"1","_type":"vertex"}"#.to_string(), |e| {
            matches!(e, IOError::UnsupportedGraphSON { .. })
        }),
        (r#"{"vertices":5}"#.to_string(), |e| {
            matches!(e, IOError::GraphSONParse { .. })
        }),
    ];
    for (text, expected) in cases {
        let err = GraphSONImport::new()
            .load(std::io::Cursor::new(text.clone()))
            .expect_err("must fail");
        assert!(expected(&err), "{text:?}: {err:?}");
        assert!(err.help().is_some());
    }
    let bytes = b"{\"id\":\"a\"}\n{\"id\":\"\xff\"}\n".to_vec();
    let err = GraphSONImport::new()
        .load(std::io::Cursor::new(bytes))
        .expect_err("must fail");
    assert!(
        matches!(err, IOError::GraphSONParse { line: 2, .. }),
        "{err}"
    );
}

#[test]
fn oversized_lines_and_documents_are_refused_before_reading_them() {
    let importer = GraphSONImport::new().with_max_text_bytes(64);
    let long = format!("{{\"id\":\"a\"}}\n{{\"id\":\"{}\"}}\n", "x".repeat(100));
    let err = importer
        .load(std::io::Cursor::new(long))
        .expect_err("must fail");
    assert!(
        matches!(
            err,
            IOError::InputTooLarge {
                line: 2,
                limit: 64,
                ..
            }
        ),
        "{err}"
    );
    // A pretty-printed wrapped document counts as a whole.
    let pretty = format!(
        "{{\n\"vertices\": [{{\"id\": \"{}\"}}]\n}}\n",
        "y".repeat(100)
    );
    let err = importer
        .load(std::io::Cursor::new(pretty))
        .expect_err("must fail");
    assert!(matches!(err, IOError::InputTooLarge { .. }), "{err}");
    // A broken first line of the line form is reported as such; the rest is never read.
    let broken = format!("{{\"id\": oops}}\n{{\"id\":\"{}\"}}\n", "w".repeat(100));
    let err = importer
        .load(std::io::Cursor::new(broken))
        .expect_err("must fail");
    assert!(
        matches!(err, IOError::GraphSONParse { line: 1, .. }),
        "{err}"
    );
    // At the limit is fine.
    let exact = format!("{{\"id\":\"{}\"}}", "z".repeat(64 - 9));
    assert_eq!(exact.len(), 64);
    assert!(importer.load(std::io::Cursor::new(exact)).is_ok());
}

// ---------------------------------------------------------------------------------------------
// Units, schema and hooks
// ---------------------------------------------------------------------------------------------

#[test]
fn a_failing_import_into_a_live_graph_leaves_nothing() {
    let mut graph = GraphSource::tinkerpop_modern();
    let before = snapshot(&graph);
    // The second line fails to parse after the first vertex was added.
    let err = graph
        .import_graphson_reader("{\"id\":\"new\"}\n{bad\n".as_bytes())
        .unwrap_err();
    assert!(matches!(err, IOError::GraphSONParse { line: 2, .. }));
    assert_eq!(snapshot(&graph), before);
    // A vertex id that already exists in the target is a graph error, also rolled back.
    let err = graph
        .import_graphson_reader("{\"id\":\"new\"}\n{\"id\":\"1\"}\n".as_bytes())
        .unwrap_err();
    assert!(matches!(err, IOError::Graph(_)), "{err}");
    assert_eq!(snapshot(&graph), before);
}

#[test]
fn the_stored_schema_coerces_and_validates_on_import() {
    let mut graph = GraphSource::empty();
    graph.write().set_schema(person_schema()).unwrap();
    // An untyped (GraphSON 1.0 style) canonical string becomes the declared uuid.
    let ok = r#"{"id":"p","label":"person","properties":{"name":[{"value":"x"}],"id_card":[{"value":"41d2e28a-20a4-4ab0-b379-d810dede3786"}]}}"#;
    graph.import_graphson_reader(ok.as_bytes()).unwrap();
    assert!(matches!(
        vertex_props(&graph, "p").get("id_card"),
        Some(ElementProperty::Uuid(_))
    ));
    // An undeclared label is rejected in closed mode: the whole import fails.
    let bad = r#"{"id":"q","label":"robot"}"#;
    assert!(graph.import_graphson_reader(bad.as_bytes()).is_err());
    assert_eq!(graph.read().vertex_count(), 1);
}

/// A closed schema with one `person` label: `name: string`, `id_card: uuid`.
fn person_schema() -> GraphSchema {
    GraphSchema::from_json(
        r#"{"mode": "closed", "vertices": {"person": {"schema": {"type": "object", "properties": {
            "name": {"type": "string"},
            "id_card": {"type": "string", "format": "uuid"}
        }}}}}"#,
    )
    .expect("schema")
}
