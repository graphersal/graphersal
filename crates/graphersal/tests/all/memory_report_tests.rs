//! The memory report (book page "Memory Usage", section "The memory report"): exact figures on
//! small graphs (modern, sparse keys and several shapes per label, multi-label vertices, nested
//! objects, vectors, long strings, compressed and shared values, property indexes, tombstones),
//! the reconciliation with `memory_usage().property_bytes`, stratified sampling within
//! tolerance, the caps, cancellation, request validation, a storage without a structure
//! (`Forwarding`), the text and value forms and the DSL.
#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::catalog::{CompressionDefinition, PropertyIndexDefinition};
use graphersal::exec::CancelToken;
use graphersal::prelude::*;
use graphersal::storage::{
    ElementKind, Graph, MemoryReport, MemoryReportRequest, PropertyMemory, PropertyTarget,
};

use crate::forwarding::Forwarding;
use crate::script_host::*;

fn props(entries: Vec<(&str, ElementProperty)>) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(key, value);
    }
    map
}

fn report_of<S: GraphStorage>(graph: &S, request: &MemoryReportRequest) -> MemoryReport {
    GraphTraversalSource::new(graph)
        .memory_report(request)
        .expect("a valid request")
}

fn full(graph: &TraversalGraph) -> MemoryReport {
    report_of(graph, &MemoryReportRequest::new())
}

fn row<'a>(
    report: &'a MemoryReport,
    element: ElementKind,
    label: Option<&str>,
    key: &str,
) -> &'a PropertyMemory {
    report
        .property(&PropertyTarget::top_level(element, label, key))
        .unwrap_or_else(|| panic!("no row {element:?} {label:?} {key}"))
}

fn vertex<'a>(report: &'a MemoryReport, label: &str, key: &str) -> &'a PropertyMemory {
    row(report, ElementKind::Vertex, Some(label), key)
}

/// The rows, label rows, shapes and dedup tables add up to `memory_usage().property_bytes`.
fn assert_reconciles(graph: &TraversalGraph, report: &MemoryReport) {
    assert_eq!(
        report.property_bytes(),
        graph.memory_usage().unwrap().property_bytes,
        "the report's property bytes differ from memory_usage()"
    );
}

/// What the stored values at `key` of the vertices labelled `label` (primary) hold: count,
/// slots and heap, read through the public API.
fn expected(graph: &TraversalGraph, label: &str, key: &str) -> (u64, u64, u64) {
    let mut out = (0, 0, 0);
    for v in graph.vertices() {
        if graph.vertex_label(v) != Some(label) {
            continue;
        }
        if let Some(value) = graph.vertex_properties(v).and_then(|map| map.get(key)) {
            out.0 += 1;
            out.1 += 32;
            out.2 += value.estimated_heap_bytes();
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Exact figures
// ---------------------------------------------------------------------------------------------

#[test]
fn modern_rows_labels_and_structure() {
    let graph = TraversalGraph::tinkerpop_modern();
    let report = full(&graph);
    assert!(!report.sampled);
    assert_eq!(report.sample_every, 1);
    assert_eq!(report.visited_elements, 12);
    assert_eq!(report.path_depth, 1);
    assert_eq!(report.properties.len(), 6);

    let name = vertex(&report, "person", "name");
    assert_eq!((name.elements, name.label_elements), (4, 4));
    assert!(name.leaf);
    assert_eq!(name.value_types.len(), 1);
    assert_eq!(
        (name.value_types[0].name.as_str(), name.value_types[0].count),
        ("string", 4)
    );
    assert_eq!(
        (name.slot_bytes, name.heap_bytes, name.bytes()),
        (128, 0, 128)
    );
    let strings = name.strings.unwrap();
    assert_eq!(strings.values, 4);
    assert_eq!(strings.inline_values, 4);
    assert_eq!(
        strings.plain_bytes,
        "marko vadas josh peter".len() as u64 - 3
    );
    assert_eq!(strings.distinct_estimate, 4);
    assert!(strings.distinct_reliable);
    // Every value is inline: one bucket, "no heap".
    let used: Vec<_> = name.sizes.iter().filter(|b| b.values > 0).collect();
    assert_eq!(used.len(), 1);
    assert_eq!(used[0].max_bytes, Some(0));

    let age = vertex(&report, "person", "age");
    assert_eq!(age.value_types[0].name, "int64");
    assert!(age.strings.is_none());
    let lang = vertex(&report, "software", "lang");
    assert_eq!(lang.strings.unwrap().distinct_estimate, 1);
    let weight = row(&report, ElementKind::Edge, Some("created"), "weight");
    assert_eq!(
        (weight.elements, weight.value_types[0].name.as_str()),
        (4, "float64")
    );

    let person = report.label(ElementKind::Vertex, Some("person")).unwrap();
    assert_eq!((person.elements, person.values), (4, 8));
    assert_eq!(person.label_bytes, 4 * 24);
    assert_eq!(person.property_bytes, 8 * 32);
    let degree = person.degree.unwrap();
    // marko: 3 out (knows x2, created); josh: 2 out, 1 in.
    assert_eq!((degree.max_out, degree.max_in, degree.max), (3, 1, 3));
    assert_eq!(degree.edge_ends, 8);
    let software = report.label(ElementKind::Vertex, Some("software")).unwrap();
    assert_eq!(software.degree.unwrap().max_in, 3);
    let created = report.label(ElementKind::Edge, Some("created")).unwrap();
    assert!(created.degree.is_none());
    assert_eq!(created.label_bytes, 0);
    // Vertices first.
    assert_eq!(report.labels[0].element, ElementKind::Vertex);
    assert_eq!(report.labels[3].element, ElementKind::Edge);

    let structure = report.structure.unwrap();
    assert_eq!(structure.totals, graph.memory_usage().unwrap());
    assert_eq!(structure.vertex_slots.live, 6);
    assert_eq!(structure.edge_slots.live, 6);
    assert_eq!(structure.vertex_slots.tombstoned, 0);
    assert_eq!(
        structure.id_index_bytes + structure.label_index_bytes,
        structure.totals.index_bytes
    );
    assert_eq!(
        person.slot_bytes,
        Some(4 * structure.vertex_slots.slot_bytes)
    );
    assert_eq!(
        report.estimated_total_bytes(),
        Some(structure.totals.total_bytes())
    );
    assert_eq!(report.shapes.shapes, 3);
    assert_eq!(report.shapes.shaped_elements, 12);
    assert_eq!(report.shapes.unshaped_elements, 0);
    assert_reconciles(&graph, &report);
}

#[test]
fn sparse_keys_several_shapes_and_value_types() {
    let mut graph = TraversalGraph::new();
    for i in 0..10i64 {
        let mut entries = vec![("name", ElementProperty::from(format!("n{i}")))];
        if i % 2 == 0 {
            entries.push(("age", ElementProperty::from(i)));
        }
        if i % 5 == 0 {
            entries.push(("note", ElementProperty::Null));
        }
        if i == 3 {
            entries.push(("age", ElementProperty::from(1.5)));
        }
        graph
            .insert_vertex(None, vec!["p".into()], props(entries))
            .unwrap();
    }
    let report = full(&graph);
    let age = vertex(&report, "p", "age");
    assert_eq!((age.elements, age.label_elements), (6, 10));
    let types: Vec<(&str, u64)> = age
        .value_types
        .iter()
        .map(|t| (t.name.as_str(), t.count))
        .collect();
    assert_eq!(types, [("int64", 5), ("float64", 1)]);
    let note = vertex(&report, "p", "note");
    assert_eq!(
        (note.elements, note.value_types[0].name.as_str()),
        (2, "null")
    );
    assert_eq!(vertex(&report, "p", "name").elements, 10);
    // Key sequences: [name, age, note] (0), [name] (1, 7, 9), [name, age] (2, 3, 4, 6, 8),
    // [name, note] (5) -> 4 distinct shapes for 10 elements.
    assert_eq!(report.shapes.shapes, 4);
    assert_eq!(report.shapes.shaped_elements, 10);
    let in_histogram: u64 = report.shapes.histogram.iter().map(|b| b.shapes).sum();
    assert_eq!(in_histogram, 4);
    assert_reconciles(&graph, &report);
}

#[test]
fn a_multi_label_vertex_counts_under_its_primary_label_only() {
    let mut graph = TraversalGraph::new();
    graph
        .insert_vertex(
            None,
            vec!["person".into(), "admin".into()],
            props(vec![("name", "ada".into())]),
        )
        .unwrap();
    graph
        .insert_vertex(None, vec!["admin".into()], props(vec![("level", 3.into())]))
        .unwrap();
    graph
        .insert_vertex(None, vec![], props(vec![("x", 1.into())]))
        .unwrap();
    let report = full(&graph);
    assert_eq!(vertex(&report, "person", "name").elements, 1);
    assert!(
        report
            .property(&PropertyTarget::top_level(
                ElementKind::Vertex,
                Some("admin"),
                "name"
            ))
            .is_none()
    );
    let admin = report.label(ElementKind::Vertex, Some("admin")).unwrap();
    assert_eq!(admin.elements, 1);
    let person = report.label(ElementKind::Vertex, Some("person")).unwrap();
    // Both labels' slots count where the vertex is attributed.
    assert_eq!(person.label_bytes, 2 * 24);
    // Unlabeled elements: label `None`.
    assert_eq!(row(&report, ElementKind::Vertex, None, "x").elements, 1);
    assert_eq!(report.label(ElementKind::Vertex, None).unwrap().elements, 1);
    assert_reconciles(&graph, &report);
}

#[test]
fn nested_objects_long_strings_and_vectors() {
    let mut graph = TraversalGraph::new();
    let long = "x".repeat(300);
    for i in 0..4 {
        let doc = props(vec![
            ("title", ElementProperty::from(format!("t{i}"))),
            ("body", ElementProperty::from(long.as_str())),
        ]);
        let mut entries = vec![
            ("doc", ElementProperty::Object(Box::new(doc))),
            ("text", ElementProperty::from(long.repeat(i + 1))),
            (
                "embedding",
                ElementProperty::Vector(Vector::from_f32(vec![0.5; 8]).unwrap()),
            ),
        ];
        if i == 0 {
            entries.push((
                "small",
                ElementProperty::Vector(Vector::from_f32(vec![1.0; 3]).unwrap()),
            ));
        }
        graph
            .insert_vertex(None, vec!["d".into()], props(entries))
            .unwrap();
    }
    let report = full(&graph);

    let doc = vertex(&report, "d", "doc");
    assert!(!doc.leaf);
    assert_eq!(doc.nested_values, 4);
    assert_eq!(doc.value_types[0].name, "object");
    let (count, slots, heap) = expected(&graph, "d", "doc");
    assert_eq!(
        (doc.elements, doc.slot_bytes, doc.heap_bytes),
        (count, slots, heap)
    );
    assert!(
        doc.heap_bytes > 4 * 300,
        "the nested body counts: {}",
        doc.heap_bytes
    );

    let text = vertex(&report, "d", "text");
    assert!(text.leaf);
    let (_, _, heap) = expected(&graph, "d", "text");
    assert_eq!(text.heap_bytes, heap);
    let strings = text.strings.unwrap();
    assert_eq!(strings.inline_values, 0);
    assert_eq!(strings.plain_bytes, 300 * (1 + 2 + 3 + 4));
    assert_eq!(strings.heap_bytes, heap);
    assert!(text.max_value_bytes >= 1200);
    // 300 B -> bucket <= 512, 600 -> <= 1 KiB, 900 and 1200 -> <= 1 KiB / <= 2 KiB.
    let bucket_sum: u64 = text.sizes.iter().map(|b| b.values).sum();
    assert_eq!(bucket_sum, 4);
    let bytes_sum: u64 = text.sizes.iter().map(|b| b.bytes).sum();
    assert_eq!(bytes_sum, text.heap_bytes);
    assert_eq!(text.above(0), (4, text.heap_bytes));
    assert_eq!(text.above(1 << 20), (0, 0));
    let (above_kib, _) = text.above(1024);
    assert_eq!(above_kib, 1, "only the 1200-byte value is above 1 KiB");

    let embedding = vertex(&report, "d", "embedding");
    assert_eq!(embedding.value_types[0].name, "vector");
    assert_eq!(embedding.vectors.len(), 1);
    let vectors = embedding.vectors[0];
    assert_eq!(vectors.element, graphersal::vector::VectorElement::F32);
    assert_eq!((vectors.dimension, vectors.count), (8, 4));
    assert_eq!(vectors.bytes, embedding.heap_bytes);
    let small = vertex(&report, "d", "small");
    assert_eq!((small.elements, small.vectors[0].dimension), (1, 3));
    assert_reconciles(&graph, &report);
}

#[test]
fn compressed_and_deduped_values_and_rules() {
    let mut graph = TraversalGraph::new();
    let cv = "Senior engineer, led the platform team. ".repeat(20);
    let paths = [
        "crates/graphersal/src/a/long/path/to/one.rs",
        "crates/graphersal/src/a/long/path/to/two.rs",
    ];
    for i in 0..20 {
        graph
            .insert_vertex(
                None,
                vec!["customer".into()],
                props(vec![
                    ("cv", ElementProperty::from(format!("{cv}{i}"))),
                    ("file", ElementProperty::from(paths[i % 2])),
                ]),
            )
            .unwrap();
    }
    let mut g = graph.traversal_mut();
    g.define_compression(
        "cv",
        CompressionDefinition::new(ElementKind::Vertex, "customer", "cv").unwrap(),
    )
    .unwrap();
    g.define_compression(
        "file",
        CompressionDefinition::new(ElementKind::Vertex, "customer", "file")
            .unwrap()
            .with_codec(graphersal::catalog::Codec::Dedup),
    )
    .unwrap();
    drop(g);
    let report = full(&graph);

    let cv_row = vertex(&report, "customer", "cv");
    let stats = graph.compression_stats("cv").unwrap();
    assert_eq!(cv_row.compressed_values, stats.compressed_values);
    assert_eq!(cv_row.compressed_values, 20);
    assert_eq!(cv_row.compressed_plain_bytes, stats.plain_bytes);
    assert_eq!(cv_row.compressed_stored_bytes, stats.stored_bytes);
    assert_eq!(cv_row.rules, ["cv"]);
    assert_eq!(cv_row.value_types[0].name, "string");
    let strings = cv_row.strings.unwrap();
    assert_eq!(strings.plain_bytes, stats.plain_bytes);
    assert!(
        (18..=22).contains(&strings.distinct_estimate),
        "{}",
        strings.distinct_estimate
    );

    let file = vertex(&report, "customer", "file");
    assert_eq!(file.deduped_values, 20);
    assert_eq!(file.heap_bytes, 0, "a shared text is counted in its table");
    assert_eq!(file.strings.unwrap().distinct_estimate, 2);
    assert_eq!(file.rules, ["file"]);

    assert_eq!(report.compression_rules.len(), 2);
    let rule = report
        .compression_rules
        .iter()
        .find(|rule| rule.name == "file")
        .unwrap();
    assert_eq!(rule.codec, "dedup");
    assert_eq!(rule.target.key(), Some("file"));
    assert_eq!(rule.stats.unwrap().compressed_values, 20);
    assert_reconciles(&graph, &report);
}

#[test]
fn every_property_index_is_reported_separately() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let mut g = graph.traversal_mut();
    g.create_index(
        "person_name",
        PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
    )
    .unwrap();
    g.create_index(
        "person_age",
        PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
    )
    .unwrap();
    drop(g);
    let report = full(&graph);
    assert_eq!(report.indexes.len(), 2);
    let total: u64 = report.indexes.iter().map(|i| i.stats.bytes).sum();
    assert_eq!(total, graph.memory_usage().unwrap().property_index_bytes);
    assert!(report.indexes.iter().all(|i| i.stats.entries == 4));
    assert_eq!(vertex(&report, "person", "name").indexes, ["person_name"]);
    assert_eq!(vertex(&report, "person", "age").indexes, ["person_age"]);
    assert!(vertex(&report, "software", "name").indexes.is_empty());
}

#[test]
fn removed_elements_leave_tombstoned_slots() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for id in ["2", "3"] {
        let v = graph.get_vertex_by_id(id).unwrap();
        graph.drop_vertex(v).unwrap();
    }
    let report = full(&graph);
    let structure = report.structure.unwrap();
    assert_eq!(structure.vertex_slots.live, 4);
    assert_eq!(structure.vertex_slots.tombstoned, 2);
    // vertex 3 had three edges (created x3), vertex 2 one (knows): four edge slots freed, those
    // below the highest live slot tombstoned, those above it spare.
    assert_eq!(structure.edge_slots.live, 2);
    assert!(structure.edge_slots.tombstoned >= 1);
    assert!(structure.edge_slots.tombstoned + structure.edge_slots.spare >= 4);
    assert_eq!(
        structure.vertex_slots.bytes(),
        (4 + 2 + structure.vertex_slots.spare) * structure.vertex_slots.slot_bytes
    );
    assert_reconciles(&graph, &report);
}

#[test]
fn dropped_keys_leave_map_overhead_that_reconciles() {
    let mut graph = TraversalGraph::new();
    for i in 0..5i64 {
        graph
            .insert_vertex(
                None,
                vec!["p".into()],
                props(vec![("a", i.into()), ("b", i.into()), ("c", i.into())]),
            )
            .unwrap();
    }
    let handles: Vec<_> = graph.vertices().collect();
    for v in handles {
        graph.drop_vertex_property("b", v).unwrap();
    }
    let report = full(&graph);
    let label = report.label(ElementKind::Vertex, Some("p")).unwrap();
    assert_eq!(label.values, 10);
    assert_eq!(label.map_overhead_bytes, 5 * 32, "one spare slot per map");
    assert_reconciles(&graph, &report);
}

// ---------------------------------------------------------------------------------------------
// Sampling, caps, cancellation, validation
// ---------------------------------------------------------------------------------------------

/// 2000 `big` vertices with a text of 100..1100 bytes, 3 `tiny` vertices, edges between them.
fn sampling_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let mut handles = Vec::new();
    for i in 0..2000usize {
        let text = "y".repeat(100 + (i * 7919) % 1000);
        handles.push(
            graph
                .insert_vertex(
                    None,
                    vec!["big".into()],
                    props(vec![("text", text.into()), ("n", (i as i64).into())]),
                )
                .unwrap(),
        );
    }
    for i in 0..3i64 {
        graph
            .insert_vertex(None, vec!["tiny".into()], props(vec![("k", i.into())]))
            .unwrap();
    }
    for i in 0..1999 {
        graph
            .insert_edge(
                None,
                Some("next".into()),
                handles[i],
                handles[i + 1],
                PropertyMap::default(),
            )
            .unwrap();
    }
    graph
}

#[test]
fn a_sample_extrapolates_per_label_within_tolerance() {
    let graph = sampling_graph();
    let exact = full(&graph);
    let sampled = report_of(&graph, &MemoryReportRequest::new().with_sample_every(10));
    assert!(sampled.sampled);
    assert_eq!(sampled.sample_every, 10);
    // 200 of the big ones, all 3 tiny ones (the first of a label is always read), 200 edges.
    assert_eq!(sampled.visited_elements, 200 + 1 + 200);

    let (exact_text, sampled_text) = (
        vertex(&exact, "big", "text"),
        vertex(&sampled, "big", "text"),
    );
    assert_eq!(sampled_text.elements, 2000);
    assert_eq!(sampled_text.label_elements, 2000);
    let error = (sampled_text.heap_bytes as f64 - exact_text.heap_bytes as f64).abs()
        / exact_text.heap_bytes as f64;
    assert!(error < 0.05, "heap bytes off by {error}");
    let distinct = sampled_text.strings.unwrap();
    assert!(!distinct.distinct_reliable);
    // A label with fewer elements than the step is still there, counted exactly.
    let tiny = vertex(&sampled, "tiny", "k");
    assert_eq!((tiny.elements, tiny.label_elements), (3, 3));
    assert_eq!(
        sampled
            .label(ElementKind::Vertex, Some("tiny"))
            .unwrap()
            .elements,
        3
    );
    let edges = sampled.label(ElementKind::Edge, Some("next")).unwrap();
    assert_eq!(edges.elements, 1999);
    let total_error = (sampled.property_bytes() as f64 - exact.property_bytes() as f64).abs()
        / exact.property_bytes() as f64;
    assert!(total_error < 0.05, "property bytes off by {total_error}");

    // `max_elements` picks the step from the element count.
    let capped = report_of(&graph, &MemoryReportRequest::new().with_max_elements(400));
    assert_eq!(capped.sample_every, (2000 + 3 + 1999u64).div_ceil(400));
}

#[test]
fn row_and_label_caps_keep_the_largest_and_the_totals() {
    let graph = sampling_graph();
    let report = report_of(
        &graph,
        &MemoryReportRequest::new()
            .with_max_rows(1)
            .with_max_labels(1),
    );
    assert_eq!(report.properties.len(), 1);
    assert_eq!(report.properties[0].target.key(), Some("text"));
    assert_eq!(report.rows_omitted, 2);
    assert_eq!(report.labels.len(), 1);
    assert_eq!(report.labels[0].label.as_deref(), Some("big"));
    assert_eq!(report.labels_omitted, 2);
    assert_reconciles(&graph, &report);
}

#[test]
fn a_cancelled_report_fails_with_cancelled() {
    let graph = sampling_graph();
    let token = CancelToken::new();
    token.cancel();
    let err = GraphTraversalSource::new(&graph)
        .memory_report(&MemoryReportRequest::new().with_cancel(token.clone()))
        .unwrap_err();
    assert!(matches!(*err, TraverserError::Cancelled { .. }), "{err:?}");
    // The thread's token applies too.
    let err = token
        .scope(|| GraphTraversalSource::new(&graph).memory_report(&MemoryReportRequest::new()))
        .unwrap_err();
    assert!(matches!(*err, TraverserError::Cancelled { .. }), "{err:?}");
}

#[test]
fn progress_is_reported_up_to_the_total() {
    let graph = sampling_graph();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    report_of(
        &graph,
        &MemoryReportRequest::new().with_progress(move |p| {
            sink.lock().unwrap().push((p.visited, p.total));
        }),
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.last(), Some(&(4002, 4002)));
}

#[test]
fn invalid_requests_are_refused() {
    let graph = TraversalGraph::tinkerpop_modern();
    let g = GraphTraversalSource::new(&graph);
    for request in [
        MemoryReportRequest::new().with_sample_every(0),
        MemoryReportRequest::new().with_max_elements(0),
        MemoryReportRequest::new().with_path_depth(2),
    ] {
        let err = g.memory_report(&request).unwrap_err().to_string();
        assert!(err.contains("memory report"), "{err}");
    }
}

// ---------------------------------------------------------------------------------------------
// Any storage, forms, DSL
// ---------------------------------------------------------------------------------------------

#[test]
fn a_storage_without_structure_gets_every_other_part() {
    let graph = TraversalGraph::tinkerpop_modern();
    let own = full(&graph);
    let forwarded = Forwarding::new(TraversalGraph::tinkerpop_modern());
    let report = report_of(&forwarded, &MemoryReportRequest::new());
    assert!(report.structure.is_none());
    assert_eq!(report.estimated_total_bytes(), None);
    assert_eq!(report.unattributed_bytes(), None);
    assert_eq!(report.properties, own.properties);
    assert_eq!(report.shapes, own.shapes);
    assert_eq!(report.labels.len(), own.labels.len());
    for label in &report.labels {
        let mine = own.label(label.element, label.label.as_deref()).unwrap();
        assert_eq!(label.slot_bytes, None);
        assert_eq!(label.property_bytes, mine.property_bytes);
        assert_eq!(label.degree, mine.degree);
    }
}

#[test]
fn text_and_value_forms() {
    let graph = TraversalGraph::tinkerpop_modern();
    let report = full(&graph);
    let text = report.to_string();
    assert!(text.starts_with("Memory report at commit 0:"), "{text}");
    assert!(text.contains("vertex person $.name"), "{text}");
    assert!(text.contains("Structure"), "{text}");
    let json = report.to_json();
    for key in [
        r#""properties":["#,
        r#""path":"$.name""#,
        r#""value_types":[{"type":"string","count":4}]"#,
        r#""structure":{"totals":"#,
        r#""sample_every":1"#,
    ] {
        assert!(json.contains(key), "{key} in {json}");
    }
}

#[test]
fn the_dsl_answers_the_report_map() {
    let shared = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
    let rows = eval_value(
        shared.clone(),
        r#"let r = g.memory_report(#{max_rows: 2}); [r.properties.len(), r.rows_omitted, r.labels.len()]"#,
    )
    .unwrap();
    assert_eq!(rows.to_string(), "[2, 4, 4]");
    let bytes = eval_value(shared.clone(), "g.memory_report().property_bytes").unwrap();
    assert_eq!(
        bytes.as_int().unwrap() as u64,
        shared.read().memory_usage().unwrap().property_bytes
    );
    for (script, needle) in [
        ("g.memory_report(#{rows: 2})", "the key 'rows'"),
        (
            "g.memory_report(#{max_rows: 0})",
            "max_rows as a positive integer",
        ),
        ("g.memory_report(5)", "a map"),
        ("g.memory_report(#{path_depth: 2})", "path depth"),
    ] {
        let err = eval_value(shared.clone(), script).unwrap_err().to_string();
        assert!(err.contains(needle), "{script}: {err}");
    }
}
