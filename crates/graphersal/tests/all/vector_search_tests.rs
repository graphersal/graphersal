//! Exact vector search (book page "Vectors"): `similarity()`, `nearest()` and `vector_mean()`
//! through the Rust API and the DSL. `nearest()` against a naive full sort for every metric and
//! element kind, ties, bulk merging on and off, skipped elements, the query vector from a
//! traversal, the schema's metric annotation as the default, saved queries, the plan
//! (`.profile()`, the label pushdown), the errors with their help, the deadline, and the recall
//! of quantized vectors against f32.

#![cfg(feature = "script")]

use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::vector::{VectorElement, VectorMetric};

use crate::script_host::{debug_value, eval_value};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A deterministic pseudo-random stream in -1..1.
fn pseudo_random(seed: u64, n: usize) -> Vec<f32> {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 40) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0) as f32
        })
        .collect()
}

fn f32v(values: Vec<f32>) -> Vector {
    Vector::from_f32(values).unwrap()
}

/// `n` vertices `doc` with ids `d0..`, a property `emb` of `element` (from random f32 values of
/// `dimension`), and every seventh vertex without `emb` (every eleventh with a string `emb`).
fn random_graph(n: usize, dimension: usize, element: VectorElement) -> Graph {
    let mut graph = TraversalGraph::new();
    for i in 0..n {
        let mut props = PropertyMap::default();
        props.insert("i", ElementProperty::from(i as i64));
        if i % 11 == 10 {
            props.insert("emb", ElementProperty::from("not a vector"));
        } else if i % 7 != 6 {
            let vector = f32v(pseudo_random(i as u64, dimension))
                .quantize(element)
                .unwrap();
            props.insert("emb", ElementProperty::Vector(vector));
        }
        graph
            .add_vertex(Some(format!("d{i}").into()), vec!["doc".into()], props)
            .unwrap();
    }
    Graph::new(graph)
}

/// The naive answer: every vertex with a vector scored, a stable sort by rank, the first `k`
/// ids.
fn naive(graph: &Graph, q: &Vector, k: usize, metric: VectorMetric) -> Vec<String> {
    let graph = graph.read();
    let mut scored: Vec<(f64, String)> = Vec::new();
    for v in graph.vertices() {
        if let Some(ElementProperty::Vector(stored)) = graph
            .vertex_properties(v)
            .and_then(|props| props.get("emb"))
        {
            let score = stored.similarity(q, metric).unwrap();
            let rank = if metric.higher_is_better() {
                -score
            } else {
                score
            };
            scored.push((rank, graph.vertex_id(v).unwrap().to_string()));
        }
    }
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    scored.into_iter().take(k).map(|(_, id)| id).collect()
}

fn ids(values: Vec<ElementProperty>) -> Vec<String> {
    values
        .into_iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn search(graph: &Graph, q: &Vector, k: usize, metric: Option<VectorMetric>) -> Vec<String> {
    let lock = graph.read();
    ids(lock
        .traversal()
        .v(None)
        .nearest("emb", q, k, metric)
        .id()
        .to_list()
        .unwrap())
}

fn run_on(graph: Arc<Graph>, script: &str) -> String {
    match eval_value(graph, script) {
        Ok(value) => debug_value(&value),
        Err(err) => panic!("{script}: {err}"),
    }
}

fn fails_on(graph: Arc<Graph>, script: &str) -> String {
    match eval_value(graph, script) {
        Ok(value) => panic!("{script} must fail, got {}", debug_value(&value)),
        // The script error's text is the full diagnostic, help included.
        Err(err) => err.to_string(),
    }
}

/// Four documents a..d with 2-dimensional vectors (c stored as i8, d without one).
fn docs() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::empty());
    run_on(
        graph.clone(),
        r#"g.add_v("doc").property("name", "a").property("emb", vector([1.0, 0.0])).iterate();
           g.add_v("doc").property("name", "b").property("emb", vector([0.6, 0.8])).iterate();
           g.add_v("doc").property("name", "c").property("emb", vector([0.0, 1.0], "i8")).iterate();
           g.add_v("doc").property("name", "d").iterate();
           g.add_v("note").property("name", "n").property("emb", vector([0.7, 0.7])).iterate();
           1"#,
    );
    graph
}

// ---------------------------------------------------------------------------------------------
// nearest(): differential against a naive sort
// ---------------------------------------------------------------------------------------------

#[test]
fn nearest_equals_a_naive_sort_for_every_metric_and_kind() {
    let dimension = 24;
    for element in [VectorElement::F32, VectorElement::F16, VectorElement::I8] {
        let graph = random_graph(300, dimension, element);
        for seed in 0..3u64 {
            let q = f32v(pseudo_random(10_000 + seed, dimension));
            for metric in [VectorMetric::Cosine, VectorMetric::Dot, VectorMetric::L2] {
                for k in [0, 1, 5, 17, 300, 1000] {
                    assert_eq!(
                        search(&graph, &q, k, Some(metric)),
                        naive(&graph, &q, k, metric),
                        "{element} {metric} k={k}"
                    );
                }
            }
            // A quantized query against stored vectors of another kind.
            let q8 = q.quantize(VectorElement::I8).unwrap();
            assert_eq!(
                search(&graph, &q8, 10, None),
                naive(&graph, &q8, 10, VectorMetric::Cosine)
            );
        }
    }
    let graph = random_graph(300, 100, VectorElement::Bit);
    let q = f32v(pseudo_random(7, 100))
        .quantize(VectorElement::Bit)
        .unwrap();
    for metric in VectorMetric::ALL {
        for k in [1, 10, 400] {
            assert_eq!(
                search(&graph, &q, k, Some(metric)),
                naive(&graph, &q, k, metric),
                "bit {metric} k={k}"
            );
        }
    }
}

#[test]
fn equal_scores_keep_the_input_order() {
    let mut graph = TraversalGraph::new();
    for i in 0..20 {
        let mut props = PropertyMap::default();
        // Two distinct vectors only: ten ties each.
        let v = if i % 2 == 0 {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        };
        props.insert("emb", ElementProperty::Vector(f32v(v)));
        graph
            .add_vertex(Some(format!("t{i:02}").into()), vec!["doc".into()], props)
            .unwrap();
    }
    let graph = Graph::new(graph);
    let q = f32v(vec![1.0, 0.1]);
    let got = search(&graph, &q, 13, None);
    let want: Vec<String> = (0..20)
        .step_by(2)
        .chain((1..20).step_by(2))
        .take(13)
        .map(|i| format!("t{i:02}"))
        .collect();
    assert_eq!(got, want);
    // Repeated runs give the same answer.
    for _ in 0..3 {
        assert_eq!(search(&graph, &q, 13, None), want);
    }
}

#[test]
fn bulk_merging_is_invisible_to_nearest() {
    // A fan-in graph: every hub reaches every leaf, so out().out() repeats each leaf many
    // times and the lazy barrier merges them into bulk traversers.
    let mut graph = TraversalGraph::new();
    let root = graph
        .add_vertex(
            Some("root".into()),
            vec!["r".into()],
            PropertyMap::default(),
        )
        .unwrap();
    let mut hubs = Vec::new();
    for h in 0..6 {
        let hub = graph
            .add_vertex(
                Some(format!("h{h}").into()),
                vec!["hub".into()],
                PropertyMap::default(),
            )
            .unwrap();
        graph
            .add_edge(None, Some("to".into()), root, hub, PropertyMap::default())
            .unwrap();
        hubs.push(hub);
    }
    for l in 0..40 {
        let mut props = PropertyMap::default();
        props.insert(
            "emb",
            ElementProperty::Vector(f32v(pseudo_random(500 + l, 8))),
        );
        let leaf = graph
            .add_vertex(Some(format!("l{l}").into()), vec!["leaf".into()], props)
            .unwrap();
        for hub in &hubs {
            graph
                .add_edge(None, Some("to".into()), *hub, leaf, PropertyMap::default())
                .unwrap();
        }
    }
    let shared = Graph::new(graph);
    let lock = shared.read();
    let q = f32v(pseudo_random(9, 8));
    // Each leaf appears 6 times; k values that cut inside a group of duplicates too.
    for k in [1, 4, 6, 7, 13, 50, 240, 300] {
        for metric in [VectorMetric::Cosine, VectorMetric::L2] {
            let run = |merge: bool| {
                let mut out = ids(lock
                    .traversal()
                    .with("bulk.merge", merge)
                    .v("root")
                    .out(None)
                    .out(None)
                    .nearest("emb", &q, k, Some(metric))
                    .id()
                    .to_list()
                    .unwrap());
                out.sort();
                out
            };
            let unmerged = run(false);
            assert_eq!(unmerged.len(), k.min(240), "k={k}");
            assert_eq!(run(true), unmerged, "k={k} {metric}");
        }
    }
    // The count of a merged search is k, cut inside the last group.
    let count = lock
        .traversal()
        .v("root")
        .out(None)
        .out(None)
        .nearest("emb", &q, 7, None)
        .count()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(count.as_i64(), Some(7));
}

// ---------------------------------------------------------------------------------------------
// The DSL
// ---------------------------------------------------------------------------------------------

#[test]
fn nearest_in_the_dsl_ranks_best_first_and_skips_elements_without_a_vector() {
    let graph = docs();
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().has_label("doc").nearest("emb", vector([1.0, 0.1]), 3).values("name").to_list()"#
        ),
        r#"["a", "b", "c"]"#
    );
    // l2: smallest distance first; a list of numbers is a query vector too.
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().nearest("emb", [0.0, 1.0], 2, #{metric: "l2"}).values("name").to_list()"#
        ),
        r#"["c", "b"]"#
    );
    // A non-vector value is skipped like a missing one.
    run_on(
        graph.clone(),
        r#"g.add_v("doc").property("name", "s").property("emb", "text").iterate(); 1"#,
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.V().hasLabel("doc").nearest("emb", vector([1.0, 0.0]), 10).values("name").toList()"#
        ),
        r#"["a", "b", "c"]"#
    );
    // A jpath key.
    run_on(
        graph.clone(),
        r#"g.add_v("deep").property("meta", #{emb: vector([0.0, -1.0])}).property("name", "x").iterate(); 1"#,
    );
    assert_eq!(
        run_on(
            graph,
            r#"g.v().has_label("deep").nearest(jpath("meta.emb"), vector([0.0, -1.0]), 1).values("name").next()"#
        ),
        r#""x""#
    );
}

#[test]
fn similarity_overloads() {
    let graph = docs();
    let check =
        |script: &str, want: &str| assert_eq!(run_on(graph.clone(), script), want, "{script}");
    // The key form, the value form, a metric, a list query.
    check(
        r#"g.v().has("name", "b").similarity("emb", vector([1.0, 0.0])).next()"#,
        "0.6000000095367428",
    );
    check(
        r#"g.v().has("name", "b").values("emb").similarity(vector([1.0, 0.0])).next()"#,
        "0.6000000095367428",
    );
    check(
        r#"g.v().has("name", "a").similarity("emb", [0.0, 0.0], "l2").next()"#,
        "1.0",
    );
    check(
        r#"g.v().has("name", "a").values("emb").similarity([1.0, 1.0], "dot").next()"#,
        "1.0",
    );
    // Elements without the vector are dropped.
    check(
        r#"g.v().has_label("doc").similarity("emb", vector([1.0, 0.0])).count().next()"#,
        "3",
    );
    // A query from a traversal, evaluated per traverser.
    check(
        r#"g.v().has("name", "a").as("me").v().has_label("doc").similarity("emb", __.select("me").values("emb")).to_list()"#,
        "[1.0, 0.6000000095367428, 0.0]",
    );
    // Ranking with order().by(similarity) equals nearest().
    check(
        r#"g.v().has_label("doc").order().by(__.similarity("emb", vector([0.0, 1.0])), Order.desc).values("name").to_list() == g.v().has_label("doc").nearest("emb", vector([0.0, 1.0]), 10).values("name").to_list()"#,
        "true",
    );
    // camelCase twin of vector_mean, project with a score.
    check(
        r#"g.v().has_label("doc").values("emb").vector_mean().next()"#,
        "vector([0.53333336, 0.6])",
    );
    check(
        r#"g.v().has_label("doc").nearest("emb", vector([1.0, 0.0]), 1).project("name", "score").by("name").by(__.similarity("emb", vector([1.0, 0.0]))).next()"#,
        r#"#{"name": "a", "score": 1.0}"#,
    );
}

#[test]
fn the_query_vector_from_a_traversal_must_be_one_vector() {
    let graph = docs();
    // More like this: the query vector of one vertex, the search over the others.
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().has("name", "a").as("me").values("emb").as("q")
                .v().has_label("doc").where(P.neq("me"))
                .nearest("emb", __.select("q"), 2).values("name").to_list()"#
        ),
        r#"["b", "c"]"#
    );
    // A query that depends on the input is refused.
    let error = fails_on(
        graph.clone(),
        r#"g.v().has_label("doc").as("a").nearest("emb", __.select("a").values("emb"), 2).to_list()"#,
    );
    assert!(
        error.contains("different vectors") && error.contains("similarity()"),
        "{error}"
    );
    // A query traversal without a result.
    let error = fails_on(
        graph,
        r#"g.v().has_label("doc").nearest("emb", __.values("missing"), 2).to_list()"#,
    );
    assert!(error.contains("no result"), "{error}");
}

#[test]
fn vector_mean_forms() {
    let graph = docs();
    let check =
        |script: &str, want: &str| assert_eq!(run_on(graph.clone(), script), want, "{script}");
    check(
        r#"g.inject(vector([1.0, 0.0]), vector([0.0, 1.0]), ()).vector_mean().next()"#,
        "vector([0.5, 0.5])",
    );
    check(
        r#"g.inject(vector([1.0, 0.0]), vector([0.0, 1.0])).fold().vector_mean().next()"#,
        "vector([0.5, 0.5])",
    );
    check(
        r#"g.inject([vector([1.0, 3.0]), vector([3.0, 1.0])]).local(__.unfold().vector_mean()).next()"#,
        "vector([2.0, 2.0])",
    );
    check(
        r#"g.v().has("missing").values("emb").vector_mean().to_list()"#,
        "[]",
    );
    // Recommendations: the taste of two liked documents, then the nearest note.
    check(
        r#"g.v().has("name", P.within("a", "c")).values("emb").fold().vector_mean().as("taste")
             .v().has_label("note").nearest("emb", __.select("taste"), 1).values("name").next()"#,
        r#""n""#,
    );
    let error = fails_on(
        graph.clone(),
        r#"g.inject(vector([1.0, 0.0]), vector([1.0])).vector_mean().next()"#,
    );
    assert!(error.contains("dimensions 2 and 1"), "{error}");
    let error = fails_on(graph, r#"g.inject(1).vector_mean().next()"#);
    assert!(error.contains("expected a vector"), "{error}");
}

// ---------------------------------------------------------------------------------------------
// The metric: argument, schema annotation, conflicts
// ---------------------------------------------------------------------------------------------

#[test]
fn the_schema_metric_annotation_is_the_default_metric() {
    let graph = docs();
    run_on(
        graph.clone(),
        r#"g.set_schema(#{mode: "none", vertices: #{
             doc: #{schema: #{type: "object", properties: #{emb: #{type: "array", format: "vector", metric: "l2"}}}},
             note: #{schema: #{type: "object", properties: #{emb: #{type: "array", format: "vector", metric: "dot"}}}}}}); 1"#,
    );
    // l2 from the schema: a distance.
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().has("name", "a").similarity("emb", [0.0, 0.0]).next()"#
        ),
        "1.0"
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().has("name", "a").values("emb").similarity([0.0, 0.0]).next()"#
        ),
        "1.0"
    );
    // A computed vector has no declaration: cosine.
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.inject(vector([1.0, 0.0])).similarity([0.0, 0.0]).next()"#
        ),
        "0.0"
    );
    // nearest(): smallest l2 distance first.
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().has_label("doc").nearest("emb", [0.9, 0.1], 3).values("name").to_list()"#
        ),
        r#"["a", "b", "c"]"#
    );
    // An explicit metric wins over the annotation.
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.v().has("name", "a").similarity("emb", [0.0, 0.0], "cosine").next()"#
        ),
        "0.0"
    );
    // Labels declaring different metrics: one search needs one metric.
    let error = fails_on(
        graph.clone(),
        r#"g.v().nearest("emb", [1.0, 0.0], 3).to_list()"#,
    );
    assert!(
        error.contains("l2") && error.contains("dot") && error.contains("#{metric:"),
        "{error}"
    );
    assert_eq!(
        run_on(
            graph,
            r#"g.v().nearest("emb", [1.0, 0.0], 1, #{metric: "cosine"}).values("name").next()"#
        ),
        r#""a""#
    );
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[test]
fn search_errors_carry_their_help() {
    let graph = docs();
    let cases: [(&str, &[&str]); 9] = [
        (
            r#"g.v().has_label("doc").nearest("emb", vector([1.0, 0.0, 0.0]), 2).to_list()"#,
            &["dimensions 2 and 3", "one dimension"],
        ),
        (
            r#"g.v().has_label("doc").nearest("emb", vector([1.0, 0.0], "bit"), 2).to_list()"#,
            &["bit vector", "quantize(\"bit\")"],
        ),
        (
            r#"g.v().has_label("doc").nearest("emb", vector([1.0, 0.0]), 2, #{metric: "hamming"}).to_list()"#,
            &["hamming", "bit vectors"],
        ),
        (
            r#"g.v().nearest("emb", vector([1.0, 0.0]), 2, #{metric: "manhattan"})"#,
            &["unknown vector metric", "\"cosine\""],
        ),
        (
            r#"g.v().nearest("emb", vector([1.0, 0.0]), 2, #{ef: 64})"#,
            &["metric"],
        ),
        (
            r#"g.v().nearest("emb", vector([1.0, 0.0]), -1)"#,
            &["k >= 0"],
        ),
        (
            r#"g.v().nearest("emb", "not a vector", 2).to_list()"#,
            &["nearest needs a schema attached"],
        ),
        (r#"g.v().similarity("emb", vector([1.0]), 5)"#, &[]),
        (
            r#"g.inject("text").similarity(vector([1.0])).next()"#,
            &["expected a vector"],
        ),
    ];
    for (script, needles) in cases {
        let error = fails_on(graph.clone(), script);
        for needle in needles {
            assert!(error.contains(needle), "{script}: {error}");
        }
    }
}

#[test]
fn a_search_stops_at_the_deadline() {
    let mut graph = TraversalGraph::new();
    for i in 0..3000 {
        let mut props = PropertyMap::default();
        props.insert("emb", ElementProperty::Vector(f32v(pseudo_random(i, 1024))));
        graph.add_vertex(None, vec!["doc".into()], props).unwrap();
    }
    let shared = Graph::new(graph);
    let lock = shared.read();
    let q = f32v(pseudo_random(1, 1024));
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v(None)
        .nearest("emb", __::constant(ElementProperty::Vector(q)), 5, None)
        .to_list()
        .unwrap_err();
    assert!(
        matches!(err.root_cause(), TraverserError::Timeout { .. }),
        "{err:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Plan, saved queries, permissions
// ---------------------------------------------------------------------------------------------

#[test]
fn the_profile_names_the_search_and_keeps_the_label_pushdown() {
    let graph = docs();
    let profile = run_on(
        graph,
        r#"g.v().has_label("doc").nearest("emb", vector([1.0, 0.0]), 2, #{metric: "l2"}).similarity("emb", __.constant(vector([0.0, 1.0]))).profile().to_string()"#,
    )
    // The debug form of a string escapes its quotes.
    .replace("\\\"", "\"");
    assert!(profile.contains(r#"v(labels: ["doc"])"#), "{profile}");
    assert!(
        profile.contains(r#"nearest("emb", vector([1.0, 0.0]), 2, #{metric: "l2"})"#),
        "{profile}"
    );
    assert!(
        profile.contains(r#"similarity("emb", __.constant(vector([0.0, 1.0])))"#),
        "{profile}"
    );
}

#[test]
fn a_property_index_serves_the_filter_before_the_search() {
    let graph = Arc::new(GraphSource::empty());
    run_on(
        graph.clone(),
        r#"g.add_v("doc").property("lang", "en").property("emb", vector([1.0, 0.0])).iterate();
           g.add_v("doc").property("lang", "de").property("emb", vector([1.0, 0.0])).iterate();
           g.create_index(#{label: "doc", key: "lang"});
           g.create_index(#{label: "doc", key: "emb"}); 1"#,
    );
    let profile = run_on(
        graph.clone(),
        r#"g.v().has_label("doc").has("lang", "en").nearest("emb", vector([1.0, 0.0]), 5).profile().to_string()"#,
    )
    .replace("\\\"", "\"");
    assert!(profile.contains("[index: vertex doc.l"), "{profile}");
    assert!(
        profile.contains("property_index") && profile.contains("nearest(\"emb\""),
        "{profile}"
    );
    // A vector operand is no index key: the has() scans, with the same result.
    assert_eq!(
        run_on(
            graph,
            r#"g.v().has_label("doc").has("emb", vector([1.0, 0.0])).nearest("emb", vector([1.0, 0.0]), 5).count().next()"#
        ),
        "2"
    );
}

#[test]
fn a_saved_query_searches_with_a_vector_parameter() {
    let graph = docs();
    run_on(
        graph.clone(),
        r#"g.define_query(#{name: "semantic_search", folder: "rag",
             params: #{q: #{schema: #{type: "array", format: "vector", dimension: 2}},
                       k: #{schema: #{type: "integer"}, default: 2}},
             body: `fn semantic_search(q, k) { g.v().has_label("doc").nearest("emb", q, k).values("name") }`}); 1"#,
    );
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.query("semantic_search", #{q: vector([0.0, 1.0])}).to_list()"#
        ),
        r#"["c", "b"]"#
    );
    // A list argument is coerced to the declared vector.
    assert_eq!(
        run_on(
            graph.clone(),
            r#"g.query("semantic_search", #{q: [1.0, 0.0], k: 1}).to_list()"#
        ),
        r#"["a"]"#
    );
    // The declared dimension is checked before the body runs.
    let error = fails_on(
        graph,
        r#"g.query("semantic_search", #{q: [1.0, 0.0, 0.0]}).to_list()"#,
    );
    assert!(error.contains("q"), "{error}");
}

#[test]
fn the_search_steps_are_read_only() {
    let graph = docs();
    // Under a read-only policy the search runs (it reads only what v() reads).
    let engine = graphersal::script::engine_with_limits(
        &graphersal::script::ScriptLimits::default(),
        Arc::new(graphersal::auth::AccessPolicy::read_only()),
    );
    let mut scope = graphersal::script::graph_scope_with_options(
        graph.clone(),
        &[],
        Arc::new(graphersal::auth::AccessPolicy::read_only()),
    );
    let value = engine
        .eval_with_scope::<graphersal::script::rhai::Dynamic>(
            &mut scope,
            r#"g.v().has_label("doc").nearest("emb", vector([1.0, 0.0]), 1).similarity("emb", vector([1.0, 0.0])).next()"#,
        )
        .unwrap();
    assert_eq!(debug_value(&value), "1.0");
}

// ---------------------------------------------------------------------------------------------
// Quantization: memory against accuracy
// ---------------------------------------------------------------------------------------------

/// The recall@10 of a quantized search against the f32 search over the same random vectors
/// (the book page "Vectors" quotes these numbers; run with `--nocapture` to see them).
#[test]
fn quantized_search_recall_against_f32() {
    let (n, dimension, k, queries) = (2000, 128, 10, 20);
    let f32_graph = random_graph(n, dimension, VectorElement::F32);
    let mut recalls = Vec::new();
    for element in [VectorElement::F16, VectorElement::I8, VectorElement::Bit] {
        let graph = random_graph(n, dimension, element);
        let mut hits = 0usize;
        for query in 0..queries {
            let q = f32v(pseudo_random(90_000 + query, dimension));
            let exact = naive(&f32_graph, &q, k, VectorMetric::Cosine);
            let (q, metric) = if element == VectorElement::Bit {
                (
                    q.quantize(VectorElement::Bit).unwrap(),
                    VectorMetric::Hamming,
                )
            } else {
                (q, VectorMetric::Cosine)
            };
            let got = search(&graph, &q, k, Some(metric));
            hits += got.iter().filter(|id| exact.contains(id)).count();
        }
        let recall = hits as f64 / (k * queries as usize) as f64;
        println!("recall@{k} of {element} against f32 ({n} x {dimension}, random): {recall:.2}");
        recalls.push((element, recall));
    }
    // Bit vectors as a prefilter: the 100 nearest by hamming, re-ranked by the f32 vectors.
    let mut both = TraversalGraph::new();
    for i in 0..n {
        let exact = f32v(pseudo_random(i as u64, dimension));
        let mut props = PropertyMap::default();
        props.insert(
            "emb_bit",
            ElementProperty::Vector(exact.quantize(VectorElement::Bit).unwrap()),
        );
        props.insert("emb", ElementProperty::Vector(exact));
        both.add_vertex(Some(format!("d{i}").into()), vec!["doc".into()], props)
            .unwrap();
    }
    let both = Graph::new(both);
    let lock = both.read();
    let mut hits = 0usize;
    for query in 0..queries {
        let q = f32v(pseudo_random(90_000 + query, dimension));
        let exact = naive(&both, &q, k, VectorMetric::Cosine);
        let got = ids(lock
            .traversal()
            .v(None)
            .nearest(
                "emb_bit",
                q.quantize(VectorElement::Bit).unwrap(),
                100,
                Some(VectorMetric::Hamming),
            )
            .nearest("emb", &q, k, None)
            .id()
            .to_list()
            .unwrap());
        hits += got.iter().filter(|id| exact.contains(id)).count();
    }
    let rerank = hits as f64 / (k * queries as usize) as f64;
    println!("recall@{k} of bit top-100 re-ranked by f32: {rerank:.2}");
    assert!(rerank >= 0.5, "{rerank}");
    // f16 is near exact, i8 close, bit a coarse prefilter.
    assert!(recalls[0].1 >= 0.95, "{recalls:?}");
    assert!(recalls[1].1 >= 0.80, "{recalls:?}");
    assert!(recalls[2].1 >= 0.10, "{recalls:?}");
}
