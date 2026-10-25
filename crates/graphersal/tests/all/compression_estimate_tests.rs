//! Estimating compression rules before defining one (book page "Compressed Properties",
//! section "Estimating the gain"; issue #12): the estimate of a candidate equals what
//! `compression_stats` reports after defining that rule on the same data (differential, below
//! the caps), the graph is unchanged by an estimate, sampling above the caps is deterministic
//! and close, cancellation, warnings, the current rule as a baseline, validation, a foreign
//! storage, and the DSL in both spellings.

use std::sync::{Arc, Mutex};

use graphersal::catalog::{
    CandidateEstimate, CompressionCandidate, CompressionDefinition, CompressionEstimate,
    CompressionEstimateRequest, DEFAULT_ESTIMATE_THRESHOLDS, Definition, EstimateWarning,
};
use graphersal::changes::{ChangeSet, CommitHook, HookError};
use graphersal::exec::CancelToken;
use graphersal::prelude::*;
use graphersal::storage::ElementKind;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// A deterministic, compressible text of about `len` bytes (multi-byte characters included).
fn text(seed: usize, len: usize) -> String {
    const PHRASES: [&str; 10] = [
        "Senior engineer with a focus on distributed systems. ",
        "Led the platform team at Acme Corp. ",
        "Built a graph database engine in Rust. ",
        "Mentored juniors and ran the hiring loop. ",
        "Cut release times by 80 %. ",
        "Speaker at RustConf. ",
        "M.Sc., Technische Universität München. ",
        "Languages: English, Deutsch, 日本語. ",
        "Skills: Rust, Go, Kubernetes, Kafka. ",
        "Hobbies: climbing, chess. ",
    ];
    let mut out = format!("#{seed}: ");
    let mut i = seed;
    while out.len() < len {
        out.push_str(PHRASES[i % PHRASES.len()]);
        i = i.wrapping_mul(7).wrapping_add(3);
    }
    let mut end = len.min(out.len());
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    out
}

/// A pseudo-random, hardly compressible string of `len` ASCII bytes.
fn noise(seed: u64, len: usize) -> String {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b'!' + (state % 90) as u8)
        })
        .collect()
}

fn props(entries: Vec<(&str, ElementProperty)>) -> PropertyMap {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(key, value);
    }
    map
}

/// `n` customers with a `bio` of mixed sizes (some short, some incompressible, some missing, a
/// few numbers), a nested `details.cv`, and `bought` edges with `notes`.
fn populate(graph: &mut TraversalGraph, n: usize) {
    for i in 0..n {
        let bio = match i % 7 {
            0 => ElementProperty::from(format!("short {i}")),
            1 => ElementProperty::from(noise(i as u64, 300 + i % 200)),
            2 => ElementProperty::from(i as i64),
            3 => ElementProperty::from(text(i, 70 + (i * 13) % 120)),
            _ => ElementProperty::from(text(i, 200 + (i * 211) % 4000)),
        };
        let mut entries = vec![("name", ElementProperty::from(format!("c{i}")))];
        if i % 11 != 5 {
            entries.push(("bio", bio));
        }
        let mut career = PropertyMap::default();
        career.insert(
            "cv",
            ElementProperty::from(text(1000 + i, 300 + (i * 97) % 2500)),
        );
        entries.push(("details", ElementProperty::Object(Box::new(career))));
        graph
            .add_vertex(
                Some(format!("c{i}").into()),
                vec!["customer".into()],
                props(entries),
            )
            .unwrap();
    }
    for i in 0..n / 2 {
        let notes = if i % 3 == 0 {
            "fast delivery".to_string()
        } else {
            text(5000 + i, 150 + (i * 31) % 900)
        };
        graph
            .add_edge_by_ids(
                Some(format!("b{i}").into()),
                Some("bought".into()),
                &format!("c{i}"),
                &format!("c{}", (i * 7 + 1) % n),
                props(vec![("notes", ElementProperty::from(notes))]),
            )
            .unwrap();
    }
}

fn graph_of(n: usize) -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    populate(&mut graph, n);
    graph
}

fn request(element: ElementKind, label: &str, path: &str) -> CompressionEstimateRequest {
    CompressionEstimateRequest::new(element, label, path).unwrap()
}

/// The numbers of a candidate that must equal `compression_stats` (no timings).
fn numbers(c: &CandidateEstimate) -> (u64, u64, u64, u64) {
    (
        c.compressed_values,
        c.plain_bytes,
        c.stored_bytes,
        c.dictionary_bytes,
    )
}

/// An estimate without its timings, for comparing two runs.
fn without_timings(mut estimate: CompressionEstimate) -> CompressionEstimate {
    estimate.elapsed_micros = 0;
    for c in &mut estimate.candidates {
        c.encode_bytes_per_sec = None;
        c.decode_bytes_per_sec = None;
    }
    estimate
}

/// For every candidate of an estimate over `(element, label, path)` of `build()`: defines that
/// rule on a fresh copy of the same data and compares `compression_stats` with the estimate.
fn assert_estimate_equals_stats(
    build: &dyn Fn() -> TraversalGraph,
    element: ElementKind,
    label: &str,
    path: &str,
) {
    let graph = build();
    let estimate = graph
        .traversal()
        .estimate_compression(&request(element, label, path))
        .unwrap();
    assert!(
        !estimate.sampled,
        "the differential test runs below the caps"
    );
    assert_eq!(
        estimate.candidates.len(),
        2 * DEFAULT_ESTIMATE_THRESHOLDS.len()
    );
    assert!(
        estimate.candidates.iter().any(|c| c.compressed_values > 0),
        "{label} {path}: the data set must compress something"
    );
    for candidate in &estimate.candidates {
        let mut defined = build();
        let rule = candidate
            .candidate
            .apply_to(CompressionDefinition::new(element, label, path).unwrap());
        defined
            .traversal_mut()
            .define_compression("trial", rule)
            .unwrap();
        let stats = defined.compression_stats("trial").unwrap();
        assert_eq!(
            numbers(candidate),
            (
                stats.compressed_values,
                stats.plain_bytes,
                stats.stored_bytes,
                stats.dictionary_bytes
            ),
            "{element:?} {label} {path} {:?}",
            candidate.candidate
        );
        assert_eq!(
            candidate.saved_bytes,
            stats.plain_bytes as i64 - stats.stored_bytes as i64 - stats.dictionary_bytes as i64
        );
        // Estimating again with the rule in place gives the same numbers (reads are logical).
        let again = defined
            .traversal()
            .estimate_compression(
                &request(element, label, path).with_candidates([candidate.candidate]),
            )
            .unwrap();
        assert_eq!(numbers(&again.candidates[0]), numbers(candidate));
        assert!(again.candidates[0].current);
        defined.check_compression_invariant().unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// 1. The estimate equals the rule (differential)
// ---------------------------------------------------------------------------------------------

#[test]
fn every_candidate_equals_the_stats_of_its_rule_on_a_small_data_set() {
    let build = || graph_of(40);
    assert_estimate_equals_stats(&build, ElementKind::Vertex, "customer", "bio");
    assert_estimate_equals_stats(&build, ElementKind::Vertex, "customer", "details.cv");
    assert_estimate_equals_stats(&build, ElementKind::Edge, "bought", "notes");
}

#[test]
fn every_candidate_equals_the_stats_of_its_rule_on_a_larger_data_set() {
    // More than 64 KiB of sampled text: the dictionary is cut where the rule cuts it.
    let build = || graph_of(400);
    let estimate = build()
        .traversal()
        .estimate_compression(&request(ElementKind::Vertex, "customer", "bio"))
        .unwrap();
    let dictionary = estimate
        .candidates
        .iter()
        .find(|c| c.candidate.dictionary)
        .unwrap();
    assert_eq!(dictionary.dictionary_bytes, 64 * 1024);
    assert_estimate_equals_stats(&build, ElementKind::Vertex, "customer", "bio");
    assert_estimate_equals_stats(&build, ElementKind::Edge, "bought", "notes");
}

#[test]
fn counts_histogram_and_best_describe_the_values() {
    let graph = graph_of(70);
    let estimate = graph
        .traversal()
        .estimate_compression(&request(ElementKind::Vertex, "customer", "bio"))
        .unwrap();
    assert_eq!(estimate.elements, 70);
    // Every 11th customer (from the 6th) has no bio; every 7th has a number.
    let missing = (0..70).filter(|i| i % 11 == 5).count() as u64;
    let numbers = (0..70).filter(|i| i % 11 != 5 && i % 7 == 2).count() as u64;
    assert_eq!(estimate.values_found, 70 - missing);
    assert_eq!(estimate.string_values, 70 - missing - numbers);
    let histogram_values: u64 = estimate.histogram.iter().map(|b| b.values).sum();
    let histogram_bytes: u64 = estimate.histogram.iter().map(|b| b.bytes).sum();
    assert_eq!(histogram_values, estimate.string_values);
    assert_eq!(histogram_bytes, estimate.string_bytes);
    assert_eq!(estimate.histogram[0].min_bytes, 0);
    assert_eq!(estimate.histogram[0].max_bytes, Some(64));
    assert_eq!(estimate.histogram.last().unwrap().max_bytes, None);
    assert_eq!(estimate.measured_values, estimate.string_values);
    assert_eq!(
        estimate.warnings,
        [EstimateWarning::NotStrings { values: numbers }]
    );
    let best = estimate.best.expect("a best candidate");
    let most = estimate
        .candidates
        .iter()
        .map(|c| c.saved_bytes)
        .max()
        .unwrap();
    assert_eq!(estimate.candidates[best].saved_bytes, most);
    assert!(
        estimate.candidates[..best]
            .iter()
            .all(|c| c.saved_bytes < most)
    );
    for c in &estimate.candidates {
        if c.compressed_values > 0 {
            assert!(c.ratio().unwrap() <= 0.9);
        } else {
            assert_eq!(c.ratio(), None);
        }
    }
    assert!(estimate.current.is_none());
}

// ---------------------------------------------------------------------------------------------
// 2. The graph is unchanged
// ---------------------------------------------------------------------------------------------

struct Recorder(Arc<Mutex<Vec<ChangeSet>>>);

impl CommitHook for Recorder {
    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        self.0.lock().unwrap().push(changes.clone());
        Ok(())
    }
}

fn content(graph: &TraversalGraph) -> Vec<ElementProperty> {
    let mut out = graph
        .traversal()
        .v(None)
        .element_map(Vec::<&str>::new())
        .to_list()
        .unwrap();
    out.extend(
        graph
            .traversal()
            .e(None)
            .element_map(Vec::<&str>::new())
            .to_list()
            .unwrap(),
    );
    out
}

#[test]
fn an_estimate_changes_nothing() {
    let mut graph = graph_of(60);
    let rule = CompressionDefinition::new(ElementKind::Vertex, "customer", "bio")
        .unwrap()
        .with_min_bytes(512);
    graph
        .traversal_mut()
        .define_compression("bio", rule)
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    graph
        .register_commit_hook(Box::new(Recorder(seen.clone())))
        .unwrap();
    let seq = graph.last_commit_seq();
    let before = content(&graph);
    let catalog: Vec<Definition> = graph.definitions().cloned().collect();
    let stats = graph.compression_stats("bio").unwrap();
    let memory = graph.memory_usage().unwrap();
    for path in ["bio", "details.cv", "name", "nothing"] {
        let estimate = graph
            .traversal()
            .estimate_compression(&request(ElementKind::Vertex, "customer", path))
            .unwrap();
        assert!(!estimate.cancelled);
    }
    graph
        .traversal()
        .estimate_compression(&request(ElementKind::Edge, "bought", "notes"))
        .unwrap();
    assert_eq!(graph.last_commit_seq(), seq);
    assert!(seen.lock().unwrap().is_empty(), "no unit was committed");
    assert_eq!(content(&graph), before);
    assert_eq!(graph.definitions().cloned().collect::<Vec<_>>(), catalog);
    assert_eq!(graph.compression_stats("bio").unwrap(), stats);
    assert_eq!(graph.memory_usage().unwrap(), memory);
    graph.check_compression_invariant().unwrap();
}

// ---------------------------------------------------------------------------------------------
// 3. The current rule, the candidates asked for, warnings, validation
// ---------------------------------------------------------------------------------------------

#[test]
fn the_current_rule_is_the_baseline_and_one_of_the_candidates() {
    let mut graph = graph_of(50);
    let rule = CompressionDefinition::new(ElementKind::Vertex, "customer", "bio")
        .unwrap()
        .with_min_bytes(300)
        .with_dictionary(false);
    graph
        .traversal_mut()
        .define_compression("bio", rule)
        .unwrap();
    let estimate = graph
        .traversal()
        .estimate_compression(&request(ElementKind::Vertex, "customer", "bio"))
        .unwrap();
    let current = estimate.current.as_ref().expect("the current rule");
    assert_eq!(current.name, "bio");
    assert_eq!(current.rule.min_bytes(), 300);
    assert_eq!(current.stats, graph.compression_stats("bio").unwrap());
    // 300 is not a default threshold: the rule's own parameters are added at the end.
    assert_eq!(estimate.candidates.len(), 11);
    let own = estimate.candidates.last().unwrap();
    assert_eq!(own.candidate, CompressionCandidate::lz4(300, false));
    assert!(own.current);
    assert_eq!(estimate.candidates.iter().filter(|c| c.current).count(), 1);
    // The rule holds exactly what the estimate of its parameters says.
    assert_eq!(
        numbers(own),
        (
            current.stats.compressed_values,
            current.stats.plain_bytes,
            current.stats.stored_bytes,
            current.stats.dictionary_bytes
        )
    );
}

#[test]
fn the_candidates_asked_for_are_measured_in_order() {
    let graph = graph_of(30);
    let asked = [
        CompressionCandidate::lz4(1024, true),
        CompressionCandidate::lz4(100, false),
        CompressionCandidate::lz4(1024, true),
    ];
    let estimate = graph
        .traversal()
        .estimate_compression(
            &request(ElementKind::Vertex, "customer", "bio").with_candidates(asked),
        )
        .unwrap();
    let measured: Vec<CompressionCandidate> =
        estimate.candidates.iter().map(|c| c.candidate).collect();
    assert_eq!(measured, asked[..2]);
    assert_eq!(
        CompressionCandidate::matching(None, Some(128), None),
        [
            CompressionCandidate::lz4(128, false),
            CompressionCandidate::lz4(128, true)
        ]
    );
    assert_eq!(
        CompressionCandidate::matching(None, None, Some(true)).len(),
        5
    );
    assert!(CompressionCandidate::matching(None, None, None).is_empty());
}

#[test]
fn warnings_name_a_useless_rule() {
    let graph = graph_of(20);
    let g = graph.traversal();
    let codes = |element, label: &str, path: &str| {
        let estimate = g
            .estimate_compression(&request(element, label, path))
            .unwrap();
        let messages: Vec<String> = estimate
            .warnings
            .iter()
            .map(|w| w.message(&estimate))
            .collect();
        (
            estimate
                .warnings
                .iter()
                .map(|w| w.code())
                .collect::<Vec<_>>(),
            messages,
        )
    };
    let (unknown, messages) = codes(ElementKind::Vertex, "custmer", "bio");
    assert_eq!(unknown, ["unknown_label"]);
    assert!(messages[0].contains("'custmer'"), "{messages:?}");
    // The edge label exists, but not as a vertex label.
    assert_eq!(
        codes(ElementKind::Vertex, "bought", "notes").0,
        ["unknown_label"]
    );
    let (none, messages) = codes(ElementKind::Vertex, "customer", "bioo");
    assert_eq!(none, ["no_values"]);
    assert!(messages[0].contains("$.bioo"), "{messages:?}");
    // `details` is an object, `name` short strings.
    assert_eq!(
        codes(ElementKind::Vertex, "customer", "details").0,
        ["not_strings"]
    );
    assert_eq!(
        codes(ElementKind::Vertex, "customer", "name").0,
        ["nothing_qualifies"]
    );
    // One short value and a dictionary candidate only: compressed, but the dictionary costs more.
    let mut tiny = TraversalGraph::new();
    tiny.add_vertex(
        None,
        vec!["doc".into()],
        props(vec![("body", ElementProperty::from(text(1, 400)))]),
    )
    .unwrap();
    let estimate = tiny
        .traversal()
        .estimate_compression(
            &request(ElementKind::Vertex, "doc", "body")
                .with_candidates([CompressionCandidate::lz4(64, true)]),
        )
        .unwrap();
    assert_eq!(estimate.candidates[0].compressed_values, 1);
    assert_eq!(estimate.best, None);
    assert_eq!(estimate.warnings, [EstimateWarning::NoSaving]);
}

#[test]
fn an_invalid_request_is_refused_like_a_rule() {
    assert!(CompressionEstimateRequest::new(ElementKind::Vertex, "a", "a[0]").is_err());
    assert!(CompressionEstimateRequest::new(ElementKind::Vertex, "a", "$").is_err());
    let graph = graph_of(5);
    let g = graph.traversal();
    let err = g
        .estimate_compression(&request(ElementKind::Vertex, "", "bio"))
        .unwrap_err();
    assert!(err.to_string().contains("label"), "{err}");
    let err = g
        .estimate_compression(
            &request(ElementKind::Vertex, "customer", "bio")
                .with_candidates([CompressionCandidate::lz4(0, false)]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("min_bytes"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// 4. Sampling and cancellation
// ---------------------------------------------------------------------------------------------

#[test]
fn above_the_caps_a_deterministic_sample_is_extrapolated() {
    let graph = graph_of(600);
    let g = graph.traversal();
    let exact = g
        .estimate_compression(&request(ElementKind::Vertex, "customer", "details.cv"))
        .unwrap();
    assert!(!exact.sampled);
    let capped =
        || request(ElementKind::Vertex, "customer", "details.cv").with_limits(100, usize::MAX);
    let sampled = g.estimate_compression(&capped()).unwrap();
    assert!(sampled.sampled);
    assert!(
        sampled.measured_values <= 100,
        "{}",
        sampled.measured_values
    );
    // Counts and the histogram are exact even when sampled.
    assert_eq!(sampled.string_values, exact.string_values);
    assert_eq!(sampled.histogram, exact.histogram);
    // The same sample every time.
    assert_eq!(
        without_timings(g.estimate_compression(&capped()).unwrap()),
        without_timings(sampled.clone())
    );
    // Close to the exact numbers.
    for (s, e) in sampled.candidates.iter().zip(&exact.candidates) {
        assert_eq!(s.candidate, e.candidate);
        assert_eq!(s.dictionary_bytes, e.dictionary_bytes);
        let close = |a: u64, b: u64| (a as f64 - b as f64).abs() <= 0.15 * b as f64 + 2.0;
        assert!(
            close(s.compressed_values, e.compressed_values),
            "{s:?} {e:?}"
        );
        assert!(close(s.plain_bytes, e.plain_bytes), "{s:?} {e:?}");
        assert!(close(s.stored_bytes, e.stored_bytes), "{s:?} {e:?}");
    }
    // A byte cap samples too.
    let by_bytes = g
        .estimate_compression(
            &request(ElementKind::Vertex, "customer", "details.cv").with_limits(usize::MAX, 50_000),
        )
        .unwrap();
    assert!(by_bytes.sampled);
}

#[test]
fn a_cancelled_estimate_stops_with_cancelled() {
    let graph = graph_of(40);
    let token = CancelToken::new();
    token.cancel();
    let raw = graph.estimate_compression(
        &request(ElementKind::Vertex, "customer", "bio").with_cancel(token.clone()),
    );
    assert!(raw.cancelled);
    assert!(raw.candidates.is_empty());
    let err = graph
        .traversal()
        .estimate_compression(
            &request(ElementKind::Vertex, "customer", "bio").with_cancel(token.clone()),
        )
        .unwrap_err();
    assert!(
        matches!(err.root_cause(), TraverserError::Cancelled { .. }),
        "{err}"
    );
    // The running thread's token applies without one in the request.
    let err = token
        .scope(|| {
            graph
                .traversal()
                .estimate_compression(&request(ElementKind::Vertex, "customer", "bio"))
        })
        .unwrap_err();
    assert!(matches!(err.root_cause(), TraverserError::Cancelled { .. }));
    token.reset();
    assert!(
        token
            .scope(|| {
                graph.traversal().estimate_compression(&request(
                    ElementKind::Vertex,
                    "customer",
                    "bio",
                ))
            })
            .is_ok()
    );
}

// ---------------------------------------------------------------------------------------------
// 5. Storages
// ---------------------------------------------------------------------------------------------

#[test]
fn a_shared_graph_and_its_read_only_view_estimate_alike() {
    use graphersal::storage::AnyGraph;
    let shared = Graph::new(graph_of(30));
    let req = request(ElementKind::Vertex, "customer", "bio");
    let through_any = AnyGraph::estimate_compression(&shared, &req).unwrap();
    let lock = shared.read();
    let view = GraphTraversalSource::new(&*lock)
        .estimate_compression(&req)
        .unwrap();
    assert_eq!(without_timings(through_any), without_timings(view));
}

// ---------------------------------------------------------------------------------------------
// 6. The DSL
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "script")]
mod dsl {
    use super::*;
    use crate::script_host::*;

    fn shared(graph: TraversalGraph) -> Arc<Graph> {
        Arc::new(Graph::new(graph))
    }

    #[test]
    fn the_dsl_works_in_both_spellings_and_changes_nothing() {
        let graph = shared(graph_of(40));
        let run = |script: &str| eval_value(graph.clone(), script).unwrap();
        let seq = graph.read().last_commit_seq();
        let all = run(r#"g.estimate_compression(#{label: "customer", path: "bio"})"#).to_string();
        for key in [
            "values_found",
            "string_values",
            "histogram",
            "candidates",
            "saved_bytes",
            "decode_bytes_per_sec",
            "warnings",
            "\"not_strings\"",
        ] {
            assert!(all.contains(key), "{key}: {all}");
        }
        assert_eq!(
            run(r#"g.estimate_compression(#{label: "customer", path: "bio"}).candidates.len()"#)
                .as_int()
                .unwrap(),
            10
        );
        // Each given key narrows the candidates.
        let one = run(
            r#"g.estimate_compression(#{element: "vertex", label: "customer", path: jpath("$.details.cv"), codec: "lz4", min_bytes: 128, dictionary: true}).candidates"#,
        )
        .to_string();
        assert_eq!(one.matches("min_bytes").count(), 1, "{one}");
        // The DSL numbers are the engine's.
        let engine = graph
            .read()
            .traversal()
            .estimate_compression(
                &request(ElementKind::Edge, "bought", "notes")
                    .with_candidates([CompressionCandidate::lz4(256, false)]),
            )
            .unwrap();
        let stored = run(
            r#"g.estimate_compression(#{element: "edge", label: "bought", path: "notes", min_bytes: 256, dictionary: false}).candidates[0].stored_bytes"#,
        );
        assert_eq!(
            stored.as_int().unwrap(),
            engine.candidates[0].stored_bytes as i64
        );
        // A rule defined from the estimate holds what it said.
        let promised = run(
            r#"let e = g.estimate_compression(#{label: "customer", path: "bio"}); let b = e.candidates[e.best];
               g.define_compression(#{name: "bio", label: "customer", path: "bio", min_bytes: b.min_bytes, dictionary: b.dictionary});
               [b.stored_bytes, g.compressions()[0].stored_bytes, g.estimate_compression(#{label: "customer", path: "bio"}).current.name]"#,
        )
        .to_string();
        let parts: Vec<&str> = promised.trim_matches(['[', ']']).split(", ").collect();
        assert_eq!(parts[0], parts[1], "{promised}");
        assert_eq!(parts[2], "\"bio\"", "{promised}");
        assert_eq!(
            graph.read().last_commit_seq(),
            seq + 1,
            "only the define committed"
        );
    }

    #[test]
    fn wrong_arguments_are_named() {
        let graph = shared(graph_of(5));
        let err = |script: &str| eval_value(graph.clone(), script).unwrap_err().to_string();
        assert!(err(r#"g.estimate_compression("bio")"#).contains("a map"));
        assert!(
            err(r#"g.estimate_compression(#{label: "customer", path: "bio", name: "x"})"#)
                .contains("only the keys")
        );
        assert!(err(r#"g.estimate_compression(#{path: "bio"})"#).contains("label"));
        assert!(err(r#"g.estimate_compression(#{label: "customer"})"#).contains("no path"));
        assert!(
            err(r#"g.estimate_compression(#{label: "customer", path: "bio", element: "node"})"#)
                .contains("\"node\"")
        );
        assert!(
            err(r#"g.estimate_compression(#{label: "customer", path: "bio", codec: "zip"})"#)
                .contains("\"zip\"")
        );
        assert!(
            err(r#"g.estimate_compression(#{label: "customer", path: "bio", min_bytes: 0})"#)
                .contains("min_bytes")
        );
        assert!(
            err(r#"g.estimate_compression(#{label: "customer", path: "a[0]"})"#).contains("index")
        );
    }

    #[test]
    fn a_saved_query_may_estimate() {
        let graph = shared(graph_of(10));
        let value = eval_value(
            graph.clone(),
            r#"g.define_query(#{name: "est", body: "fn est() { g.estimate_compression(#{label: \"customer\", path: \"bio\"}).values_found }"});
               g.query("est")"#,
        )
        .unwrap();
        assert!(value.as_int().unwrap() > 0, "{value}");
        let listed = eval_value(graph.clone(), r#"g.queries()[0].writes"#).unwrap();
        assert!(!listed.as_bool().unwrap(), "an estimate is no write");
    }
}
