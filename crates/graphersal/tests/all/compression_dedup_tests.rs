//! The `dedup` codec of compression rules (issue #13; book page "Compressed Properties",
//! section "Deduplicating repeated values"): every distinct long value at a rule's path is
//! stored once in the rule's table and elements keep a reference. Reads stay plain, query
//! results never change, files hold plain values. Covers the estimate == stats differential
//! (vertex, edge, nested path), the invariant after writes, overwrites, drops, label changes,
//! rollbacks, recompress, codec changes and loads (snapshot and Store), the table sweep,
//! equality/hash/order of shared values, validation, memory, property indexes, exports, change
//! sets, permissions and the DSL.
#![cfg(feature = "script")]

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use graphersal::auth::AccessPolicy;
use graphersal::catalog::{
    CandidateEstimate, Codec, CompressionCandidate, CompressionDefinition,
    CompressionEstimateRequest, DEFAULT_DEDUP_ESTIMATE_THRESHOLDS, DEFAULT_DEDUP_MIN_BYTES,
    Definition, DefinitionKind, EstimateWarning,
};
use graphersal::changes::{ChangeSet, CommitHook, HookError, Mutation};
use graphersal::prelude::*;
use graphersal::storage::ElementKind;

use crate::script_host::*;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// One of `kinds` long, repeated source paths (about 50 to 60 bytes).
fn source_path(k: usize) -> String {
    format!("crates/graphersal/src/traversal/step/map_steps/step_number_{k}.rs")
}

fn props(entries: Vec<(&str, ElementProperty)>) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(key, value);
    }
    map
}

fn object(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
    ElementProperty::Object(Box::new(props(entries)))
}

/// `n` symbols: `path` repeated among 7 long values (some short, some numbers, some
/// missing), a nested `loc.file` among 3 long values, a unique `doc` per symbol, and `calls`
/// edges with a `site` repeated among 4 long values (one short).
fn populate(graph: &mut TraversalGraph, n: usize) {
    for i in 0..n {
        let path = match i % 9 {
            0 => ElementProperty::from("lib.rs"),
            1 => ElementProperty::from(i as i64),
            _ => ElementProperty::from(source_path(i % 7)),
        };
        let mut entries = vec![("name", ElementProperty::from(format!("s{i}")))];
        if i % 13 != 6 {
            entries.push(("path", path));
        }
        entries.push((
            "loc",
            object(vec![
                (
                    "file",
                    ElementProperty::from(format!(
                        "/home/user/projects/graphersal/crates/file_{}.rs",
                        i % 3
                    )),
                ),
                ("line", ElementProperty::from(i as i64)),
            ]),
        ));
        entries.push((
            "doc",
            ElementProperty::from(format!("The documentation of symbol number {i}, unique.")),
        ));
        graph
            .add_vertex(
                Some(format!("s{i}").into()),
                vec!["symbol".into()],
                props(entries),
            )
            .unwrap();
    }
    for i in 0..n / 2 {
        let site = match i % 4 {
            0 => "here".to_string(),
            k => format!("https://example.org/graphersal/call/site/{k}/index.html"),
        };
        graph
            .add_edge_by_ids(
                Some(format!("c{i}").into()),
                Some("calls".into()),
                &format!("s{i}"),
                &format!("s{}", (i * 5 + 1) % n),
                props(vec![("site", ElementProperty::from(site))]),
            )
            .unwrap();
    }
}

fn graph_of(n: usize) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    populate(&mut graph, n);
    graph
}

fn dedup_rule(element: ElementKind, label: &str, path: &str) -> CompressionDefinition {
    CompressionDefinition::new(element, label, path)
        .unwrap()
        .with_codec(Codec::Dedup)
}

/// Every dedup rule of the fixture: `path`, `loc.file` and the edges' `site`.
fn define_rules(graph: &mut TraversalGraph) {
    let mut g = graph.traversal_mut();
    g.define_compression("path", dedup_rule(ElementKind::Vertex, "symbol", "path"))
        .unwrap();
    g.define_compression(
        "file",
        dedup_rule(ElementKind::Vertex, "symbol", "loc.file"),
    )
    .unwrap();
    g.define_compression("site", dedup_rule(ElementKind::Edge, "calls", "site"))
        .unwrap();
}

fn deduped_graph(n: usize) -> TraversalGraph {
    let mut graph = graph_of(n);
    define_rules(&mut graph);
    graph
}

fn shared(graph: TraversalGraph) -> Arc<Graph> {
    Arc::new(Graph::new(graph))
}

fn invariant(graph: &TraversalGraph) {
    if let Err(reason) = graph.check_compression_invariant() {
        panic!("invariant broken: {reason}");
    }
}

/// The logical content of a graph by id (shared values compare as their strings).
fn content(graph: &TraversalGraph) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in graph.vertices() {
        let mut props: Vec<(String, String)> = graph
            .vertex_properties(v)
            .unwrap()
            .iter()
            .map(|(k, v)| (k.to_string(), format!("{:?}", v.to_logical())))
            .collect();
        props.sort();
        out.push(format!(
            "v {} {:?} {props:?}",
            graph.vertex_id(v).unwrap(),
            graph.vertex_labels(v)
        ));
    }
    for e in graph.edges() {
        let mut props: Vec<(String, String)> = graph
            .edge_properties(e)
            .map(|p| {
                p.iter()
                    .map(|(k, v)| (k.to_string(), format!("{:?}", v.to_logical())))
                    .collect()
            })
            .unwrap_or_default();
        props.sort();
        out.push(format!(
            "e {} {:?} {props:?}",
            graph.edge_id(e).unwrap(),
            graph.edge_label(e)
        ));
    }
    out.sort();
    out
}

/// The stats numbers of a rule as a tuple (values, plain, stored, dictionary, distinct).
fn stats_of(graph: &TraversalGraph, name: &str) -> (u64, u64, u64, u64, u64) {
    let s = graph.compression_stats(name).unwrap();
    (
        s.compressed_values,
        s.plain_bytes,
        s.stored_bytes,
        s.dictionary_bytes,
        s.distinct_values,
    )
}

fn numbers(c: &CandidateEstimate) -> (u64, u64, u64, u64, u64) {
    (
        c.compressed_values,
        c.plain_bytes,
        c.stored_bytes,
        c.dictionary_bytes,
        c.distinct_values,
    )
}

/// Whether the stored value of vertex `id` at top-level `key` is a shared (dedup) value.
fn is_shared(graph: &TraversalGraph, id: &str, key: &str) -> bool {
    let v = graph.get_vertex_by_id(id).unwrap();
    matches!(
        graph.vertex_properties(v).unwrap().get(key),
        Some(ElementProperty::Deduped(_))
    )
}

/// Asserts that every table holds only what elements reference (after a sweep).
fn tables_are_tight(graph: &TraversalGraph) {
    for definition in graph.definitions() {
        let Some(rule) = definition.as_compression() else {
            continue;
        };
        if rule.codec() != Codec::Dedup {
            continue;
        }
        let name = definition.name();
        assert_eq!(
            graph.dedup_table_len(name).unwrap() as u64,
            graph.compression_stats(name).unwrap().distinct_values,
            "table of {name}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 1. Representation and defaults
// ---------------------------------------------------------------------------------------------

#[test]
fn a_dedup_rule_shares_long_repeated_values_and_reads_them_plain() {
    assert_eq!(std::mem::size_of::<ElementProperty>(), 32);
    let graph = deduped_graph(60);
    invariant(&graph);
    // s2 has a long path: shared; s0 has "lib.rs" (inline, never shared); s1 a number.
    assert!(is_shared(&graph, "s2", "path"));
    assert!(!is_shared(&graph, "s0", "path"));
    assert!(!is_shared(&graph, "s1", "path"));
    assert!(!is_shared(&graph, "s2", "doc"), "no rule on doc");
    // Two elements with the same text point to one copy: 7 distinct paths at most.
    let stats = graph.compression_stats("path").unwrap();
    assert!(stats.compressed_values > 30, "{stats:?}");
    assert_eq!(stats.distinct_values, 7, "{stats:?}");
    assert_eq!(stats.dictionary_bytes, 0);
    assert!(stats.stored_bytes < stats.plain_bytes / 4, "{stats:?}");
    // Reads are plain.
    let read = graph
        .traversal()
        .v(vec!["s2"])
        .values("path")
        .to_list()
        .unwrap();
    assert_eq!(read.len(), 1);
    assert!(!read[0].holds_compressed());
    assert_eq!(read[0], ElementProperty::from(source_path(2)));
    let plain = graph_of(60);
    for script in [
        r#"g.v().has_label("symbol").values("path").to_list()"#,
        r#"g.v().has_label("symbol").values("loc").to_list()"#,
        r#"g.v().has_label("symbol").order().by("path").values("name").to_list()"#,
        r#"g.v().has_label("symbol").values("path").dedup().to_list()"#,
        r#"g.v().has_label("symbol").group_count().by("path").to_list()"#,
        r#"g.v().has("path", "crates/graphersal/src/traversal/step/map_steps/step_number_3.rs").values("name").to_list()"#,
        r#"g.v().has("path", TextP.containing("number_4")).count().to_list()"#,
        r#"g.v().values("path").is(P.gt("crates/graphersal/src/traversal/step/map_steps/step_number_2.rs")).count().to_list()"#,
        r#"g.e().has_label("calls").values("site").to_list()"#,
        r#"g.v().has_label("symbol").value_map().to_list()"#,
        r#"g.e().has_label("calls").values("site").length().to_list()"#,
        r#"g.e().has_label("calls").values("site").to_upper().to_list()"#,
    ] {
        let with = eval_value(shared(deduped_graph(60)), script).unwrap();
        let without = eval_value(shared(graph_of(60)), script).unwrap();
        assert_eq!(format!("{with:?}"), format!("{without:?}"), "{script}");
    }
    assert_eq!(content(&graph), content(&plain));

    // The defaults of a dedup rule.
    let rule = dedup_rule(ElementKind::Vertex, "symbol", "path");
    assert_eq!(rule.min_bytes(), DEFAULT_DEDUP_MIN_BYTES);
    assert!(!rule.dictionary());
    assert_eq!(rule.codec().name(), "dedup");
    assert_eq!(Codec::from_name("dedup"), Some(Codec::Dedup));
    // A threshold given before the codec is kept.
    let rule = CompressionDefinition::new(ElementKind::Vertex, "symbol", "path")
        .unwrap()
        .with_min_bytes(40)
        .with_codec(Codec::Dedup);
    assert_eq!(rule.min_bytes(), 40);
    // The payload round trips and states the effective parameters.
    let payload = rule.to_payload();
    assert_eq!(payload.get("codec"), Some(&ElementProperty::from("dedup")));
    assert_eq!(
        payload.get("dictionary"),
        Some(&ElementProperty::Boolean(false))
    );
    assert_eq!(CompressionDefinition::from_payload(&payload).unwrap(), rule);
    let json = Definition::compression("p", rule.clone()).to_json();
    assert_eq!(
        Definition::from_json(&json).unwrap(),
        Definition::compression("p", rule)
    );
}

#[test]
fn shared_values_compare_hash_and_order_as_their_strings() {
    let graph = deduped_graph(30);
    let s2 = graph.get_vertex_by_id("s2").unwrap();
    // s11 holds source_path(4).
    let s11 = graph.get_vertex_by_id("s11").unwrap();
    let stored = graph
        .vertex_properties(s2)
        .unwrap()
        .get("path")
        .unwrap()
        .clone();
    assert!(matches!(stored, ElementProperty::Deduped(_)));
    let plain = ElementProperty::from(source_path(2));
    let hash = |value: &ElementProperty| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    };
    assert_eq!(stored, plain);
    assert_eq!(plain, stored);
    assert_eq!(hash(&stored), hash(&plain));
    assert_eq!(stored.clone(), stored);
    assert_eq!(stored.to_logical(), plain);
    assert!(matches!(stored.to_logical(), ElementProperty::String(_)));
    assert_eq!(stored.text().as_deref(), Some(source_path(2).as_str()));
    assert!(stored.is_text());
    assert!(stored.holds_compressed());
    let other = graph.vertex_properties(s11).unwrap().get("path").unwrap();
    assert_ne!(&stored, other);
    assert_ne!(stored, ElementProperty::from("lib.rs"));
    // A shared value against an lz4-compressed one of the same text.
    let mut lz4 = TraversalGraph::new();
    lz4.traversal_mut()
        .define_compression(
            "p",
            CompressionDefinition::new(ElementKind::Vertex, "x", "p")
                .unwrap()
                .with_min_bytes(1)
                .with_dictionary(false),
        )
        .unwrap();
    let long = "abcabcabc".repeat(20);
    let v = lz4
        .add_vertex(
            None,
            vec!["x".into()],
            props(vec![("p", long.as_str().into())]),
        )
        .unwrap();
    let compressed = lz4.vertex_properties(v).unwrap().get("p").unwrap().clone();
    assert!(matches!(compressed, ElementProperty::Compressed(_)));
    let mut dedup = TraversalGraph::new();
    dedup
        .traversal_mut()
        .define_compression("p", dedup_rule(ElementKind::Vertex, "x", "p"))
        .unwrap();
    let w = dedup
        .add_vertex(
            None,
            vec!["x".into()],
            props(vec![("p", long.as_str().into())]),
        )
        .unwrap();
    let deduped = dedup
        .vertex_properties(w)
        .unwrap()
        .get("p")
        .unwrap()
        .clone();
    assert!(matches!(deduped, ElementProperty::Deduped(_)));
    assert_eq!(compressed, deduped);
    assert_eq!(deduped, compressed);
    assert_eq!(hash(&compressed), hash(&deduped));
    // Order: a query sorting by the shared values equals the plain one (checked above), and a
    // range predicate on them works.
    let n = graph
        .traversal()
        .v(None)
        .has_label("symbol")
        .values("path")
        .is(P::Gte(PValue::from(source_path(5).as_str())))
        .count()
        .next()
        .unwrap();
    let m = graph_of(30)
        .traversal()
        .v(None)
        .has_label("symbol")
        .values("path")
        .is(P::Gte(PValue::from(source_path(5).as_str())))
        .count()
        .next()
        .unwrap();
    assert_eq!(format!("{n:?}"), format!("{m:?}"));
}

// ---------------------------------------------------------------------------------------------
// 2. The estimate equals the rule (differential)
// ---------------------------------------------------------------------------------------------

fn dedup_request(element: ElementKind, label: &str, path: &str) -> CompressionEstimateRequest {
    CompressionEstimateRequest::new(element, label, path)
        .unwrap()
        .with_candidates(CompressionCandidate::matching(
            Some(Codec::Dedup),
            None,
            None,
        ))
}

fn assert_dedup_estimate_equals_stats(
    build: &dyn Fn() -> TraversalGraph,
    element: ElementKind,
    label: &str,
    path: &str,
) {
    let graph = build();
    let estimate = graph
        .traversal()
        .estimate_compression(&dedup_request(element, label, path))
        .unwrap();
    assert_eq!(
        estimate.candidates.len(),
        DEFAULT_DEDUP_ESTIMATE_THRESHOLDS.len()
    );
    assert!(
        estimate.candidates.iter().any(|c| c.compressed_values > 0),
        "{label} {path}: the data set must share something"
    );
    for candidate in &estimate.candidates {
        assert_eq!(candidate.candidate.codec, Codec::Dedup);
        let mut defined = build();
        let rule = candidate
            .candidate
            .apply_to(CompressionDefinition::new(element, label, path).unwrap());
        defined
            .traversal_mut()
            .define_compression("trial", rule)
            .unwrap();
        assert_eq!(
            numbers(candidate),
            stats_of(&defined, "trial"),
            "{element:?} {label} {path} {:?}",
            candidate.candidate
        );
        let (_, plain, stored, _, _) = stats_of(&defined, "trial");
        assert_eq!(candidate.saved_bytes, plain as i64 - stored as i64);
        // Estimating again with the rule in place gives the same numbers.
        let again = defined
            .traversal()
            .estimate_compression(
                &CompressionEstimateRequest::new(element, label, path)
                    .unwrap()
                    .with_candidates([candidate.candidate]),
            )
            .unwrap();
        assert_eq!(numbers(&again.candidates[0]), numbers(candidate));
        assert!(again.candidates[0].current);
        defined.check_compression_invariant().unwrap();
        tables_are_tight(&defined);
    }
}

#[test]
fn every_dedup_candidate_equals_the_stats_of_its_rule() {
    let build = || graph_of(80);
    assert_dedup_estimate_equals_stats(&build, ElementKind::Vertex, "symbol", "path");
    assert_dedup_estimate_equals_stats(&build, ElementKind::Vertex, "symbol", "loc.file");
    assert_dedup_estimate_equals_stats(&build, ElementKind::Edge, "calls", "site");
    // The unique doc saves nothing (costs at 25 bytes, nothing reaches 64), and the estimate
    // says why.
    assert_dedup_estimate_equals_stats(&build, ElementKind::Vertex, "symbol", "doc");
    let estimate = build()
        .traversal()
        .estimate_compression(&dedup_request(ElementKind::Vertex, "symbol", "doc"))
        .unwrap();
    assert!(estimate.candidates.iter().all(|c| c.saved_bytes <= 0));
    assert!(estimate.candidates[0].saved_bytes < 0);
    assert_eq!(estimate.best, None);
    assert!(
        estimate
            .warnings
            .iter()
            .any(|w| matches!(w, EstimateWarning::HighCardinality { .. })),
        "{:?}",
        estimate.warnings
    );
    let warning = estimate
        .warnings
        .iter()
        .find(|w| w.code() == "high_cardinality")
        .unwrap();
    assert!(warning.message(&estimate).contains("distinct"));
}

#[test]
fn dedup_and_lz4_side_by_side_and_dedup_is_exact_above_the_caps() {
    let graph = graph_of(200);
    let both = CompressionEstimateRequest::new(ElementKind::Vertex, "symbol", "path")
        .unwrap()
        .with_candidates(CompressionCandidate::matching_codecs(
            &[Codec::Lz4, Codec::Dedup],
            None,
            None,
        ));
    let estimate = graph.traversal().estimate_compression(&both).unwrap();
    assert_eq!(estimate.candidates.len(), 10 + 4);
    let best = &estimate.candidates[estimate.best.unwrap()];
    assert_eq!(
        best.candidate.codec,
        Codec::Dedup,
        "repeated paths: dedup wins"
    );
    assert_eq!(best.candidate.min_bytes, 25);
    assert!(
        estimate.warnings.is_empty()
            || !estimate
                .warnings
                .iter()
                .any(|w| w.code() == "high_cardinality")
    );
    // Sampling (tiny caps) changes the lz4 numbers, never the dedup ones.
    let sampled = graph
        .traversal()
        .estimate_compression(&both.clone().with_limits(5, 200))
        .unwrap();
    assert!(sampled.sampled);
    for (a, b) in estimate.candidates.iter().zip(&sampled.candidates) {
        if a.candidate.codec == Codec::Dedup {
            assert_eq!(numbers(a), numbers(b));
        }
    }
    // A dedup rule in place is the current candidate of the default estimate (appended).
    let mut defined = graph_of(200);
    defined
        .traversal_mut()
        .define_compression("path", dedup_rule(ElementKind::Vertex, "symbol", "path"))
        .unwrap();
    let estimate = defined
        .traversal()
        .estimate_compression(
            &CompressionEstimateRequest::new(ElementKind::Vertex, "symbol", "path").unwrap(),
        )
        .unwrap();
    assert_eq!(estimate.candidates.len(), 11);
    let own = estimate.candidates.last().unwrap();
    assert_eq!(own.candidate, CompressionCandidate::dedup(25));
    assert!(own.current);
    let current = estimate.current.as_ref().unwrap();
    assert_eq!(current.stats.distinct_values, own.distinct_values);
    assert_eq!(current.stats.stored_bytes, own.stored_bytes);
}

// ---------------------------------------------------------------------------------------------
// 3. The invariant after every kind of write, and the table sweep
// ---------------------------------------------------------------------------------------------

#[test]
fn the_invariant_holds_after_every_kind_of_write() {
    let graph = shared(deduped_graph(40));
    let long = "https://example.org/a/very/long/repeated/value/for/dedup";
    let steps = [
        format!(r#"g.v("s2").property("path", "{long}").to_list()"#),
        format!(r#"g.v("s3").property("path", "{long}").to_list()"#),
        r#"g.v("s3").property("path", "short").to_list()"#.to_string(),
        r#"g.v("s4").property("path", 42).to_list()"#.to_string(),
        format!(r#"g.v("s5").property(jpath("loc.file"), "{long}").to_list()"#),
        r#"g.v("s5").property(jpath("loc.file"), "tiny").to_list()"#.to_string(),
        format!(r#"g.v("s6").property("loc", #{{file: "{long}", line: 1}}).to_list()"#),
        format!(r#"g.add_v("symbol").property("name", "new").property("path", "{long}").to_list()"#),
        r#"g.v("s7").remove_property("path").to_list()"#.to_string(),
        r#"g.v("s8").remove_property(jpath("loc.file")).to_list()"#.to_string(),
        r#"g.v("s9").drop().to_list()"#.to_string(),
        format!(r#"g.merge_v(#{{"T.label": "symbol", name: "merged"}}).option(Merge.onCreate, #{{path: "{long}"}}).to_list()"#),
        format!(r#"g.e("c1").property("site", "{long}").to_list()"#),
        r#"g.e("c1").property("site", "edge").to_list()"#.to_string(),
        format!(r#"g.v("s10").add_e("calls").to(__.v("s11")).property("site", "{long}").to_list()"#),
        r#"g.define_compression(#{name: "path", label: "symbol", path: "path", codec: "dedup", min_bytes: 60})"#.to_string(),
        r#"g.define_compression(#{name: "path", label: "symbol", path: "path", codec: "lz4", min_bytes: 30, dictionary: false})"#.to_string(),
        r#"g.define_compression(#{name: "path", label: "symbol", path: "path", codec: "dedup"})"#.to_string(),
        r#"g.recompress("path")"#.to_string(),
        r#"g.drop_compression("file")"#.to_string(),
        r#"g.define_compression(#{name: "file2", label: "symbol", path: "loc.file", codec: "dedup", min_bytes: 1})"#.to_string(),
        r#"g.drop_compression("site")"#.to_string(),
    ];
    for step in &steps {
        let _ = eval_value(graph.clone(), step).unwrap_or_else(|err| panic!("{step}: {err}"));
        invariant(&graph.read());
    }
    // The logical content equals the same history without rules.
    let plain = shared(graph_of(40));
    for step in &steps {
        if !step.contains("compression") && !step.contains("recompress") {
            let _ = eval_value(plain.clone(), step).unwrap();
        }
    }
    assert_eq!(content(&graph.read()), content(&plain.read()));
}

#[test]
fn a_label_change_shares_and_unshares() {
    let mut graph = deduped_graph(20);
    let long = source_path(1);
    let v = graph
        .add_vertex(
            Some("x".into()),
            vec!["note".into()],
            props(vec![("path", long.as_str().into())]),
        )
        .unwrap();
    assert!(!is_shared(&graph, "x", "path"));
    graph.add_vertex_label(v, "symbol".into()).unwrap();
    assert!(is_shared(&graph, "x", "path"));
    invariant(&graph);
    graph.remove_vertex_label(v, "symbol").unwrap();
    assert!(!is_shared(&graph, "x", "path"));
    invariant(&graph);
}

#[test]
fn overwrites_never_grow_a_table_without_bound() {
    let mut graph = deduped_graph(10);
    let s2 = graph.get_vertex_by_id("s2").unwrap();
    for i in 0..3000 {
        graph
            .set_vertex_property(
                s2,
                "path".into(),
                format!("a long value that is overwritten again and again, number {i}").into(),
            )
            .unwrap();
        let live = graph.compression_stats("path").unwrap().distinct_values as usize;
        let len = graph.dedup_table_len("path").unwrap();
        assert!(
            len <= (2 * live).max(256) + 1,
            "{i}: {len} entries, {live} live"
        );
    }
    invariant(&graph);
    // recompress keeps only what elements reference.
    graph.traversal_mut().recompress("path").unwrap();
    tables_are_tight(&graph);
    invariant(&graph);
}

#[test]
fn rollbacks_restore_the_data_the_invariant_and_sweep_the_tables() {
    let oracle = content(&deduped_graph(40));
    let distinct_before = deduped_graph(40)
        .compression_stats("path")
        .unwrap()
        .distinct_values;

    // A failing traversal writing new long values rolls back.
    let graph = shared(deduped_graph(40));
    let failed = eval_value(
        graph.clone(),
        r#"g.v("s2", "s3").property("path", "a brand new long value that is rolled back again").values("name").as_number().to_list()"#,
    );
    assert!(failed.is_err());
    invariant(&graph.read());
    tables_are_tight(&graph.read());
    assert_eq!(content(&graph.read()), oracle);

    // A transaction mixing writes and rule changes, rolled back (nested savepoint included).
    let Ok(graph) = Arc::try_unwrap(graph) else {
        panic!("the graph is still shared")
    };
    let mut graph = graph.into_inner();
    type Failure = Box<dyn std::error::Error>;
    let result: Result<(), Failure> = graph.transaction(|g| {
        for i in 0..20 {
            g.traversal_mut()
                .v(vec![format!("s{}", i + 2)])
                .property(
                    "path",
                    format!("value {i} of a transaction that fails later").as_str(),
                )
                .to_list()?;
        }
        g.set_definition(Definition::compression(
            "path",
            dedup_rule(ElementKind::Vertex, "symbol", "path").with_min_bytes(58),
        ))?;
        g.remove_definition(DefinitionKind::COMPRESSION, "site")?;
        let inner: Result<(), Failure> = g.transaction(|g| {
            g.remove_definition(DefinitionKind::COMPRESSION, "file")?;
            let s20 = g.get_vertex_by_id("s20").unwrap();
            g.drop_vertex(s20)?;
            Err("inner".into())
        });
        assert!(inner.is_err());
        invariant(g);
        Err("outer".into())
    });
    assert!(result.is_err());
    invariant(&graph);
    tables_are_tight(&graph);
    assert_eq!(content(&graph), oracle);
    assert_eq!(graph.definitions().count(), 3);
    assert_eq!(
        graph.compression_stats("path").unwrap().distinct_values,
        distinct_before
    );
}

// ---------------------------------------------------------------------------------------------
// 4. Persistence: plain on disk, shared again after a load
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "persist")]
#[test]
fn a_snapshot_holds_plain_text_and_a_load_shares_again() {
    use graphersal::persist::{Codec as ChunkCodec, WriteOptions, write_snapshot};
    let graph = deduped_graph(50);
    let mut bytes = Vec::new();
    write_snapshot(
        &graph,
        &mut bytes,
        &WriteOptions::new().with_codec(ChunkCodec::None),
    )
    .unwrap();
    // The text is on disk once per element (plain), not once per rule.
    let needle = source_path(3);
    let occurrences = bytes
        .windows(needle.len())
        .filter(|w| *w == needle.as_bytes())
        .count() as u64;
    assert!(occurrences > 3, "{occurrences}");
    let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    invariant(&loaded);
    tables_are_tight(&loaded);
    assert_eq!(content(&loaded), content(&graph));
    for name in ["path", "file", "site"] {
        assert_eq!(stats_of(&loaded, name), stats_of(&graph, name), "{name}");
    }
}

#[cfg(feature = "persist")]
#[test]
fn a_store_replays_and_loads_a_dedup_rule() {
    use graphersal::persist::{MemDir, RecoveryTarget, Store, StoreOptions};
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    {
        let graph = store.graph();
        let mut graph = graph.write();
        for i in 0..30 {
            graph
                .traversal_mut()
                .add_v("symbol")
                .property("path", source_path(i % 4).as_str())
                .to_list()
                .unwrap();
        }
        graph
            .traversal_mut()
            .define_compression("path", dedup_rule(ElementKind::Vertex, "symbol", "path"))
            .unwrap();
        graph
            .traversal_mut()
            .add_v("symbol")
            .property("path", source_path(9).as_str())
            .to_list()
            .unwrap();
    }
    let expected = stats_of(&store.graph().read(), "path");
    assert_eq!(expected.0, 31);
    assert_eq!(expected.4, 5);
    let oracle = content(&store.graph().read());
    store.close().unwrap();
    // WAL replay.
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    invariant(&store.graph().read());
    assert_eq!(stats_of(&store.graph().read(), "path"), expected);
    assert_eq!(content(&store.graph().read()), oracle);
    // Checkpoint, then a load from the snapshot.
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let store = Store::open(&dir, StoreOptions::new()).unwrap();
    invariant(&store.graph().read());
    tables_are_tight(&store.graph().read());
    assert_eq!(stats_of(&store.graph().read(), "path"), expected);
    assert_eq!(content(&store.graph().read()), oracle);
    store.close().unwrap();
    let read_only = Store::open_read_only(&dir, RecoveryTarget::Latest).unwrap();
    invariant(&read_only.graph);
    assert_eq!(stats_of(&read_only.graph, "path"), expected);
}

// ---------------------------------------------------------------------------------------------
// 5. Validation
// ---------------------------------------------------------------------------------------------

#[test]
fn a_dictionary_with_dedup_is_refused_with_a_help_that_names_the_fix() {
    let mut graph = graph_of(10);
    let bad = dedup_rule(ElementKind::Vertex, "symbol", "path").with_dictionary(true);
    let err = graph
        .traversal_mut()
        .define_compression("path", bad.clone())
        .unwrap_err();
    let diagnostic = graphersal::error::format_diagnostic(&*err);
    assert!(diagnostic.contains("dictionary"), "{diagnostic}");
    assert!(diagnostic.contains("dedup"), "{diagnostic}");
    assert!(
        diagnostic.contains(r#"codec: "dedup""#),
        "the help shows a runnable fix: {diagnostic}"
    );
    assert_eq!(graph.definitions().count(), 0);
    let err = Definition::compression("p", bad).validate().unwrap_err();
    assert!(
        matches!(&err, graphersal::error::DefinitionError::InvalidField { field, .. } if field == "dictionary"),
        "{err:?}"
    );
    // The help's example runs.
    let shared = shared(graph_of(10));
    let _ = eval_value(
        shared.clone(),
        r#"g.define_compression(#{name: "symbol_path", label: "symbol", path: "path", codec: "dedup"})"#,
    )
    .unwrap();
    // The DSL refuses the combination and an unknown codec.
    for script in [
        r#"g.define_compression(#{name: "z", label: "symbol", path: "doc", codec: "dedup", dictionary: true})"#,
        r#"g.define_compression(#{name: "z", label: "symbol", path: "doc", codec: "zip"})"#,
        r#"g.estimate_compression(#{label: "symbol", path: "doc", codec: "dedup", dictionary: true})"#,
        r#"g.estimate_compression(#{label: "symbol", path: "doc", codec: ["lz4", 1]})"#,
        r#"g.estimate_compression(#{label: "symbol", path: "doc", codec: ["zip"]})"#,
    ] {
        let err = eval_value(shared.clone(), script).unwrap_err().to_string();
        assert!(!err.is_empty(), "{script}");
    }
    assert_eq!(shared.read().definitions().count(), 1);
    // Two rules on one path stay a conflict across codecs.
    let err = eval_value(
        shared.clone(),
        r#"g.define_compression(#{name: "other", label: "symbol", path: "path"})"#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("already covers"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// 6. Memory, property indexes, exports, change sets, permissions, DSL
// ---------------------------------------------------------------------------------------------

#[test]
fn memory_usage_counts_each_shared_text_once() {
    let plain = graph_of(400);
    let graph = deduped_graph(400);
    let without = plain.memory_usage().unwrap();
    let with = graph.memory_usage().unwrap();
    assert_eq!(without.deduped_values, 0);
    assert_eq!(without.dedup_table_bytes, 0);
    let references: u64 = ["path", "file", "site"]
        .iter()
        .map(|n| graph.compression_stats(n).unwrap().compressed_values)
        .sum();
    assert_eq!(with.deduped_values, references);
    let distinct_bytes: u64 = ["path", "file", "site"]
        .iter()
        .map(|n| {
            let s = graph.compression_stats(n).unwrap();
            // stored = distinct texts + 32 bytes each; the table counts 16 per text plus slots.
            s.stored_bytes - 16 * s.distinct_values
        })
        .sum();
    assert!(with.dedup_table_bytes >= distinct_bytes, "{with:?}");
    assert!(with.dedup_table_bytes < distinct_bytes + 4096, "{with:?}");
    assert!(
        with.property_bytes + with.deduped_plain_bytes / 2 < without.property_bytes,
        "{with:?} vs {without:?}"
    );
    assert_eq!(
        with.total_bytes(),
        with.structure_bytes
            + with.index_bytes
            + with.property_bytes
            + with.compression_dictionary_bytes
            + with.property_index_bytes
    );
}

#[test]
fn the_dsl_memory_usage_reports_the_dedup_counters() {
    let graph = shared(deduped_graph(400));
    let stats = graph.read().memory_usage().unwrap();
    assert!(stats.deduped_values > 0, "{stats:?}");
    assert!(stats.deduped_plain_bytes > 0, "{stats:?}");
    assert!(stats.dedup_table_bytes > 0, "{stats:?}");
    assert!(stats.dedup_table_bytes <= stats.property_bytes, "{stats:?}");
    let reported = eval_value(
        graph.clone(),
        r#"let m = g.memory_usage();
           [m.deduped_values, m.deduped_plain_bytes, m.dedup_table_bytes, m.property_bytes]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(
        reported,
        format!(
            "[{}, {}, {}, {}]",
            stats.deduped_values,
            stats.deduped_plain_bytes,
            stats.dedup_table_bytes,
            stats.property_bytes
        )
    );
    // Without a dedup rule the keys are present and zero.
    let plain = eval_value(
        shared(graph_of(40)),
        r#"let m = g.memory_usage(); [m.deduped_values, m.deduped_plain_bytes, m.dedup_table_bytes]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(plain, "[0, 0, 0]");
}

#[test]
fn a_property_index_on_a_dedup_path_still_serves() {
    let query = |graph: TraversalGraph| {
        let shared = shared(graph);
        let names = eval_value(
            shared.clone(),
            &format!(
                r#"g.v().has_label("symbol").has("path", "{}").values("name").to_list()"#,
                source_path(4)
            ),
        )
        .unwrap();
        let profile = eval_value(
            shared,
            &format!(
                r#"g.v().has_label("symbol").has("path", "{}").count().profile().to_string()"#,
                source_path(4)
            ),
        )
        .unwrap()
        .to_string();
        (format!("{names:?}"), profile)
    };
    let index_first = {
        let mut graph = graph_of(60);
        graph
            .traversal_mut()
            .set_definition(Definition::property_index(
                "symbol_path",
                graphersal::catalog::PropertyIndexDefinition::new(
                    ElementKind::Vertex,
                    "symbol",
                    "path",
                ),
            ))
            .unwrap();
        define_rules(&mut graph);
        graph
    };
    let rules_first = {
        let mut graph = deduped_graph(60);
        graph
            .traversal_mut()
            .set_definition(Definition::property_index(
                "symbol_path",
                graphersal::catalog::PropertyIndexDefinition::new(
                    ElementKind::Vertex,
                    "symbol",
                    "path",
                ),
            ))
            .unwrap();
        graph
    };
    let (plain_names, _) = query(graph_of(60));
    for graph in [index_first, rules_first] {
        invariant(&graph);
        let (names, profile) = query(graph);
        assert_eq!(names, plain_names);
        assert!(profile.contains("index"), "{profile}");
    }
}

#[cfg(feature = "io")]
#[test]
fn graphson_and_graphml_exports_equal_the_exports_without_rules() {
    use graphersal::io::GraphImport;
    let export = |graph: &TraversalGraph| {
        let mut graphson = Vec::new();
        graph
            .traversal()
            .export_graphson_writer(&mut graphson)
            .unwrap();
        let mut graphml = Vec::new();
        graph
            .traversal()
            .export_graphml_writer(&mut graphml)
            .unwrap();
        (graphson, graphml)
    };
    let (son_plain, ml_plain) = export(&graph_of(40));
    let (son, ml) = export(&deduped_graph(40));
    assert_eq!(son, son_plain);
    assert_eq!(ml, ml_plain);
    // An import into a graph with rules shares.
    let mut target = TraversalGraph::new();
    define_rules(&mut target);
    target.import_graphson_reader(son.as_slice()).unwrap();
    invariant(&target);
    assert_eq!(content(&target), content(&graph_of(40)));
    assert_eq!(
        stats_of(&target, "path"),
        stats_of(&deduped_graph(40), "path")
    );
}

struct Recorder(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }
}

#[test]
fn change_sets_carry_plain_values_and_replay_shares_again() {
    let mut graph = graph_of(20);
    let seen = Arc::new(Mutex::new(Vec::new()));
    graph
        .register_commit_hook(Box::new(Recorder(seen.clone())))
        .unwrap();
    define_rules(&mut graph);
    let long = source_path(5);
    graph
        .traversal_mut()
        .v(vec!["s2"])
        .property("path", long.as_str())
        .to_list()
        .unwrap();
    graph
        .traversal_mut()
        .v(vec!["s3"])
        .drop()
        .to_list()
        .unwrap();
    graph
        .traversal_mut()
        .v(vec!["s4"])
        .remove_property(vec!["path"])
        .to_list()
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 6);
    let plain = |value: &ElementProperty| !value.holds_compressed();
    for changes in seen.iter() {
        for mutation in changes.mutations() {
            match mutation {
                Mutation::SetProperty { before, after, .. } => {
                    assert!(plain(after), "{mutation:?}");
                    assert!(before.as_ref().is_none_or(plain), "{mutation:?}");
                }
                Mutation::RemoveProperty { before, .. } => assert!(plain(before)),
                Mutation::DropVertex { before, .. } => {
                    assert!(before.properties.values().all(plain))
                }
                Mutation::DropEdge { before, .. } => {
                    assert!(before.properties.values().all(plain))
                }
                _ => {}
            }
        }
    }
    #[cfg(feature = "serde")]
    {
        let json = serde_json::to_string(&seen[3]).unwrap();
        assert!(json.contains(&long), "{json}");
    }
    // A replica replaying the commits shares the same values.
    let mut replica = graph_of(20);
    for changes in seen.iter() {
        replica = replica.replay(changes).unwrap();
    }
    invariant(&replica);
    assert_eq!(content(&replica), content(&graph));
    assert_eq!(stats_of(&replica, "path"), stats_of(&graph, "path"));
}

#[test]
fn read_only_policies_cannot_define_a_dedup_rule() {
    let mut graph = deduped_graph(10);
    let policy: Arc<dyn graphersal::auth::Authorizer> = Arc::new(AccessPolicy::read_only());
    let mut g = graph.traversal_mut().with_authorizer(policy.clone());
    assert!(
        g.define_compression("x", dedup_rule(ElementKind::Vertex, "symbol", "doc"))
            .is_err()
    );
    assert!(g.recompress("path").is_err());
    assert!(g.compression_stats("path").unwrap().is_some());
    // Estimating dedup is a read.
    assert!(
        g.estimate_compression(&dedup_request(ElementKind::Vertex, "symbol", "doc"))
            .is_ok()
    );
}

#[test]
fn the_dsl_defines_lists_and_estimates_dedup_in_both_spellings() {
    let graph = shared(graph_of(60));
    let listed = eval_value(
        graph.clone(),
        r#"g.define_compression(#{name: "path", label: "symbol", path: "path", codec: "dedup"});
           let r = g.compressions()[0];
           [r.codec, r.min_bytes, r.dictionary, r.distinct_values]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(listed, r#"["dedup", 25, false, 7]"#);
    invariant(&graph.read());
    let estimate = eval_value(
        graph.clone(),
        r#"let e = g.estimate_compression(#{label: "symbol", path: "loc.file", codec: ["lz4", "dedup"]});
           let b = e.candidates[e.best];
           [e.candidates.len(), b.codec, b.distinct_values]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(estimate, r#"[14, "dedup", 3]"#);
    let only = eval_value(
        graph.clone(),
        r#"g.estimate_compression(#{label: "symbol", path: "doc", codec: "dedup"}).warnings.map(|w| w.code)"#,
    )
    .unwrap()
    .to_string();
    assert!(only.contains("high_cardinality"), "{only}");
    // A rule defined from the estimate holds what it said.
    let promised = eval_value(
        graph.clone(),
        r#"let e = g.estimate_compression(#{label: "symbol", path: "loc.file", codec: "dedup"}); let b = e.candidates[e.best];
           g.define_compression(#{name: "file", label: "symbol", path: "loc.file", codec: b.codec, min_bytes: b.min_bytes});
           let r = g.compressions().filter(|r| r.name == "file")[0];
           [b.stored_bytes == r.stored_bytes, b.distinct_values == r.distinct_values]"#,
    )
    .unwrap()
    .to_string();
    assert_eq!(promised, "[true, true]");
    invariant(&graph.read());
}
