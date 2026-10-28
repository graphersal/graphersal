//! Embedders (book page "Vectors", section "Embeddings"): the frozen `hash-ngram/1` (golden
//! bits, a literal reference implementation of the algorithm, its properties), the `embed` step
//! and the function `embed(text)` with a mock embedder (batching, order, bulk, rollback, wrong
//! answers, cancellation, a schema-declared target, replay never calling an embedder), the
//! registry rules, and a host embedder built fallibly from a file through the public API only
//! (the shape of a model-backed embedder).

#![cfg(feature = "script")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use graphersal::exec::CancelToken;
use graphersal::prelude::*;
use graphersal::script::rhai::{Dynamic, EvalAltResult};
use graphersal::vector::{
    EmbedContext, EmbedError, Embedder, EmbedderInfo, Embedders, HashNgram, Vector, VectorElement,
    VectorMetric,
};

include!("../data/hash_ngram_golden.rs");

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// Runs `script` on `graph` with `embedders` given to both the engine and the scope.
fn run(
    graph: &Arc<Graph>,
    embedders: &Arc<Embedders>,
    script: &str,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_embedders(&mut engine, embedders.clone());
    let mut scope = graphersal::script::graph_scope(graph.clone());
    graphersal::script::set_scope_embedders(&mut scope, embedders.clone());
    engine.eval_with_scope::<Dynamic>(&mut scope, script)
}

fn run_ok(graph: &Arc<Graph>, embedders: &Arc<Embedders>, script: &str) -> Dynamic {
    run(graph, embedders, script).unwrap_or_else(|err| panic!("{script}: {err}"))
}

fn run_err(graph: &Arc<Graph>, embedders: &Arc<Embedders>, script: &str) -> String {
    match run(graph, embedders, script) {
        Ok(value) => panic!("{script} should fail, gave {value:?}"),
        Err(err) => err.to_string(),
    }
}

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn default_registry() -> Arc<Embedders> {
    Arc::new(Embedders::default())
}

fn f32s(vector: &Vector) -> Vec<f32> {
    vector.to_f32_vec()
}

fn hash_ngram(texts: &[&str]) -> Result<Vec<Vector>, EmbedError> {
    HashNgram::new().embed(texts, &EmbedContext::new())
}

// ---------------------------------------------------------------------------------------------
// hash-ngram/1: the frozen algorithm
// ---------------------------------------------------------------------------------------------

/// The algorithm of the spec, written literally (strings and concatenation), against which the
/// allocation-free implementation is checked.
fn reference(text: &str, dimension: usize) -> Option<Vec<f32>> {
    fn hash(bytes: &[u8]) -> u64 {
        let mut x = 0xcbf2_9ce4_8422_2325u64;
        for byte in bytes {
            x = (x ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        x ^= x >> 30;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        x
    }
    let lower: String = text.chars().flat_map(char::to_lowercase).collect();
    let mut counts = vec![0i64; dimension];
    let mut add = |feature: Vec<u8>| {
        let x = hash(&feature);
        let index = (x % dimension as u64) as usize;
        counts[index] += if x >> 63 == 0 { 1 } else { -1 };
    };
    for word in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let mut feature = b"W".to_vec();
        feature.extend_from_slice(word.as_bytes());
        add(feature);
        let bracketed: Vec<char> = format!("<{word}>").chars().collect();
        for n in 3..=5 {
            if bracketed.len() < n {
                continue;
            }
            for window in bracketed.windows(n) {
                let gram: String = window.iter().collect();
                let mut feature = b"N".to_vec();
                feature.extend_from_slice(gram.as_bytes());
                add(feature);
            }
        }
    }
    let sum: f64 = counts.iter().map(|c| (*c as f64) * (*c as f64)).sum();
    if sum == 0.0 {
        return None;
    }
    let norm = sum.sqrt();
    Some(counts.iter().map(|c| (*c as f64 / norm) as f32).collect())
}

#[test]
fn hash_ngram_golden_vectors_are_frozen() {
    let texts: Vec<&str> = HASH_NGRAM_GOLDEN.iter().map(|(text, _)| *text).collect();
    let vectors = hash_ngram(&texts).unwrap();
    let actual: Vec<(&str, u64)> = texts
        .iter()
        .zip(&vectors)
        .map(|(text, vector)| (*text, hash_ngram_bits_hash(&f32s(vector))))
        .collect();
    assert_eq!(actual, HASH_NGRAM_GOLDEN.to_vec());
}

#[test]
fn hash_ngram_matches_the_literal_algorithm() {
    let texts = [
        "graph",
        "Graph Database",
        "a",
        "ab",
        "abc",
        "Привет, мир",
        "İstanbul ẞtraße ǅungla",
        "emoji 🚀 rocket 👩‍👩‍👧",
        "e\u{301}cole école",
        "x1 y2 z3 4 55 666",
        "tab\tnew\nline--dash__under",
        "日本語のテキスト",
    ];
    for dimension in [1, 2, 7, 256, 1000] {
        let embedder = HashNgram::with_dimension("test/hash", dimension).unwrap();
        let vectors = embedder.embed(&texts, &EmbedContext::new()).unwrap();
        for (text, vector) in texts.iter().zip(&vectors) {
            let expected = reference(text, dimension);
            let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
            match expected {
                Some(expected) => assert_eq!(
                    bits(&f32s(vector)),
                    bits(&expected),
                    "{text:?} at dimension {dimension}"
                ),
                None => panic!("{text:?}: the reference has no features"),
            }
        }
    }
}

#[test]
fn hash_ngram_properties() {
    let v = hash_ngram(&["Graphersal", "graphersal", "grahpersal", "pineapple"]).unwrap();
    // Case-insensitive.
    assert_eq!(v[0], v[1]);
    // Unit length within f32 rounding.
    let norm: f64 = f32s(&v[0])
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "{norm}");
    // A typo is closer than an unrelated word.
    let typo = v[0].similarity(&v[2], VectorMetric::Cosine).unwrap();
    let other = v[0].similarity(&v[3], VectorMetric::Cosine).unwrap();
    assert!(typo > other, "{typo} <= {other}");
    // The built-in's info.
    let info = HashNgram::new().info().clone();
    assert_eq!(info.name, "hash-ngram/1");
    assert_eq!(info.model, "hash-ngram/1");
    assert_eq!(
        (info.dimension, info.element, info.metric, info.max_batch),
        (256, VectorElement::F32, VectorMetric::Cosine, 256)
    );
    assert!(info.deterministic && info.local);
    let host = HashNgram::with_dimension("acme/hash", 64).unwrap();
    assert_eq!(host.info().model, "hash-ngram/1?dim=64");
    assert!(HashNgram::with_dimension("acme/hash", 0).is_err());
}

#[test]
fn hash_ngram_has_no_features_for_empty_and_separator_texts() {
    for text in ["", "   ", "...!!!", "— – ·"] {
        assert_eq!(
            hash_ngram(&["ok", text]).unwrap_err(),
            EmbedError::no_features(1),
            "{text:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// A mock embedder
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ok,
    Fail,
    WrongCount,
    WrongDimension,
    /// Cancels the token, then reports through `ctx.check()`.
    CancelSelf,
    /// A new value every call (not deterministic).
    // Only the `persist` test constructs it; gating it would also gate the `call` counter.
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    Random,
}

struct Mock {
    info: EmbedderInfo,
    mode: Mode,
    calls: AtomicUsize,
    seen: Mutex<Vec<Vec<String>>>,
    cancel: CancelToken,
}

impl Mock {
    fn new(name: &str, max_batch: usize, mode: Mode) -> Arc<Mock> {
        Arc::new(Mock {
            info: EmbedderInfo::new(name, 3, VectorElement::F32).with_max_batch(max_batch),
            mode,
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            cancel: CancelToken::new(),
        })
    }

    fn vector_of(text: &str) -> Vector {
        let sum: u32 = text.chars().map(|c| c as u32).sum();
        Vector::from_f32(vec![text.chars().count() as f32, (sum % 1000) as f32, 1.0]).unwrap()
    }

    fn batches(&self) -> Vec<usize> {
        self.seen.lock().unwrap().iter().map(Vec::len).collect()
    }
}

impl Embedder for Mock {
    fn info(&self) -> &EmbedderInfo {
        &self.info
    }

    fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .unwrap()
            .push(texts.iter().map(|t| t.to_string()).collect());
        match self.mode {
            Mode::Ok => Ok(texts.iter().map(|t| Mock::vector_of(t)).collect()),
            Mode::Fail => Err(EmbedError::failed("the mock model is broken")),
            Mode::WrongCount => Ok(vec![Mock::vector_of("x")]),
            Mode::WrongDimension => Ok(texts
                .iter()
                .map(|_| Vector::from_f32(vec![1.0, 2.0]).unwrap())
                .collect()),
            Mode::CancelSelf => {
                self.cancel.cancel();
                ctx.check()?;
                Ok(texts.iter().map(|t| Mock::vector_of(t)).collect())
            }
            Mode::Random => Ok(texts
                .iter()
                .map(|_| Vector::from_f32(vec![call as f32 + 1.0, 7.0, 1.0]).unwrap())
                .collect()),
        }
    }
}

fn registry_with(mock: &Arc<Mock>) -> Arc<Embedders> {
    let embedder: Arc<dyn Embedder> = mock.clone();
    Arc::new(Embedders::builder().with(embedder).unwrap().build())
}

// ---------------------------------------------------------------------------------------------
// The step and the function
// ---------------------------------------------------------------------------------------------

#[test]
fn the_step_calls_once_per_batch_and_keeps_the_order() {
    let mock = Mock::new("mock", 4, Mode::Ok);
    let registry = registry_with(&mock);
    let texts: Vec<String> = (0..10)
        .map(|i| format!("text {i}{}", "x".repeat(i)))
        .collect();
    let inject = texts
        .iter()
        .map(|t| format!("{t:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let result = run_ok(
        &modern(),
        &registry,
        &format!("g.inject({inject}).embed(#{{embedder: \"mock\"}}).to_list()"),
    );
    assert_eq!(mock.batches(), vec![4, 4, 2]);
    let vectors: Vec<Vector> = result
        .into_array()
        .unwrap()
        .into_iter()
        .map(|v| {
            v.cast::<graphersal::script::ScriptVector>()
                .vector()
                .clone()
        })
        .collect();
    let expected: Vec<Vector> = texts.iter().map(|t| Mock::vector_of(t)).collect();
    assert_eq!(vectors, expected);
}

#[test]
fn the_step_embeds_each_traverser_once_whatever_its_bulk() {
    let mock = Mock::new("mock", 64, Mode::Ok);
    let registry = registry_with(&mock);
    let count = run_ok(
        &modern(),
        &registry,
        r#"g.inject("a", "a", "a", "b").barrier().embed(#{embedder: "mock"}).count().next()"#,
    );
    assert_eq!(count.as_int().unwrap(), 4);
    assert_eq!(mock.batches(), vec![2]);
}

#[test]
fn the_key_form_reads_the_property_and_drops_elements_without_it() {
    let registry = default_registry();
    let graph = modern();
    // `lang` exists on the two software vertices only.
    let count = run_ok(&graph, &registry, r#"g.v().embed("lang").count().next()"#);
    assert_eq!(count.as_int().unwrap(), 2);
    // The key form equals values(key).embed() and the function embed(text).
    let same = run_ok(
        &graph,
        &registry,
        r#"let a = g.v().has("name", "josh").embed("name").next();
           let b = g.v().has("name", "josh").values("name").embed().next();
           a == b && a == embed("josh")"#,
    );
    assert!(same.as_bool().unwrap());
    // A jpath key.
    let nested = run_ok(
        &graph,
        &registry,
        r#"g.inject(#{doc: #{title: "Graph"}}).embed(jpath("doc.title")).next() == embed("graph")"#,
    );
    assert!(nested.as_bool().unwrap());
    // The Rust API.
    let g = graph.read();
    let vectors = g
        .traversal()
        .v(None::<()>)
        .embed("name", Some("hash-ngram/1"))
        .to_list()
        .unwrap();
    assert_eq!(vectors.len(), 6);
    assert!(vectors.iter().all(|v| v.as_vector().is_some()));
}

#[test]
fn the_profile_names_the_resolved_embedder() {
    let profile = run_ok(
        &modern(),
        &default_registry(),
        r#"g.v().has_label("person").embed("name").profile().to_string()"#,
    );
    let text = profile.into_string().unwrap();
    assert!(
        text.contains(r#"embed("name", embedder: "hash-ngram/1")"#),
        "{text}"
    );
}

#[test]
fn a_failing_embedder_rolls_the_traversal_back() {
    let mock = Mock::new("broken", 8, Mode::Fail);
    let registry = registry_with(&mock);
    let graph = modern();
    let err = run_err(
        &graph,
        &registry,
        r#"g.add_v("doc").property("t", "text").property("emb", __.embed("t", #{embedder: "broken"})).iterate()"#,
    );
    assert!(err.contains("the mock model is broken"), "{err}");
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn wrong_answers_are_bad_output() {
    for mode in [Mode::WrongCount, Mode::WrongDimension] {
        let mock = Mock::new("liar", 8, mode);
        let registry = registry_with(&mock);
        let err = run_err(
            &modern(),
            &registry,
            r#"g.inject("a", "b").embed(#{embedder: "liar"}).to_list()"#,
        );
        assert!(err.contains("returned a wrong answer"), "{err}");
        if mode == Mode::WrongDimension {
            let err = run_err(&modern(), &registry, r#"embed("a", #{embedder: "liar"})"#);
            assert!(err.contains("returned a wrong answer"), "{err}");
        }
    }
}

#[test]
fn a_cancelled_embedding_is_the_execution_cancelled() {
    let mock = Mock::new("cancel", 8, Mode::CancelSelf);
    let registry = registry_with(&mock);
    let graph = modern();
    let token = mock.cancel.clone();
    let result = token.scope(|| {
        graph
            .read()
            .traversal()
            .with_embedders(registry.clone())
            .v(None::<()>)
            .embed("name", Some("cancel"))
            .to_list()
    });
    let err = result.unwrap_err();
    assert!(
        matches!(err.root_cause(), TraverserError::Cancelled { .. }),
        "{err}"
    );
}

#[test]
fn a_schema_declared_target_checks_the_embedding() {
    let graph = modern();
    let registry = default_registry();
    let _ = run_ok(
        &graph,
        &registry,
        r#"g.set_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object",
             properties: #{emb: #{type: "array", format: "vector", element: "f32",
                                  dimension: 3}}}}}})"#,
    );
    let err = run_err(
        &graph,
        &registry,
        r#"g.v().has_label("person").property("emb", __.embed("name")).iterate()"#,
    );
    assert!(err.contains("dimension"), "{err}");
    assert!(
        run_ok(
            &graph,
            &registry,
            r#"g.v().has_label("person").has("emb").count().next()"#
        )
        .as_int()
        .unwrap()
            == 0
    );
}

#[test]
fn an_unknown_embedder_fails_before_the_run() {
    let err = run_err(
        &modern(),
        &default_registry(),
        r#"g.v().embed("name", #{embedder: "nope"}).to_list()"#,
    );
    assert!(err.contains("there is no embedder \"nope\""), "{err}");
    let err = run_err(
        &modern(),
        &default_registry(),
        r#"embed("a", #{embedder: "nope"})"#,
    );
    assert!(err.contains("there is no embedder \"nope\""), "{err}");
    let err = run_err(
        &modern(),
        &default_registry(),
        r#"g.inject("a").embed(#{model: 1})"#,
    );
    assert!(err.contains("the key \"model\""), "{err}");
}

#[test]
fn not_text_is_refused_without_conversion() {
    let err = run_err(
        &modern(),
        &default_registry(),
        r#"g.v().values("age").embed().to_list()"#,
    );
    assert!(err.contains("embed() takes a string, not int64"), "{err}");
    let fixed = run_ok(
        &modern(),
        &default_registry(),
        r#"g.v().values("age").as_string().embed().count().next()"#,
    );
    assert_eq!(fixed.as_int().unwrap(), 4);
}

#[test]
fn g_embedders_lists_the_registry() {
    let mock = Mock::new("acme/mock", 8, Mode::Ok);
    let registry = registry_with(&mock);
    let names = run_ok(
        &modern(),
        &registry,
        r#"g.embedders().map(|e| e.name + " " + e.model + " " + e.dimension + " " + e.default)"#,
    );
    let names: Vec<String> = names
        .into_array()
        .unwrap()
        .into_iter()
        .map(|d| d.into_string().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "hash-ngram/1 hash-ngram/1 256 true".to_string(),
            "acme/mock acme/mock 3 false".to_string()
        ]
    );
}

#[cfg(feature = "persist")]
#[test]
fn replay_and_recovery_never_call_an_embedder() {
    use graphersal::persist::{MemDir, Store, StoreOptions};
    let mock = Mock::new("random", 8, Mode::Random);
    let registry = registry_with(&mock);
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    let graph = store.graph().clone();
    let _ = run_ok(
        &graph,
        &registry,
        r#"g.add_v("doc").property("t", "hello").property("emb", __.embed("t", #{embedder: "random"})).iterate()"#,
    );
    let stored = run_ok(&graph, &registry, r#"g.v().values("emb").next()"#);
    store.close().unwrap();
    let calls = mock.calls.load(Ordering::SeqCst);
    assert_eq!(calls, 1);
    let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
    let again = run_ok(reopened.graph(), &registry, r#"g.v().values("emb").next()"#);
    assert_eq!(mock.calls.load(Ordering::SeqCst), calls);
    assert_eq!(
        stored.cast::<graphersal::script::ScriptVector>(),
        again.cast::<graphersal::script::ScriptVector>()
    );
}

// ---------------------------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------------------------

struct Fixed(EmbedderInfo);

impl Embedder for Fixed {
    fn info(&self) -> &EmbedderInfo {
        &self.0
    }
    fn embed(&self, texts: &[&str], _: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
        Ok(texts
            .iter()
            .map(|_| Vector::from_f32(vec![1.0; self.0.dimension]).unwrap())
            .collect())
    }
}

fn fixed(info: EmbedderInfo) -> Arc<dyn Embedder> {
    Arc::new(Fixed(info))
}

fn registry_error(result: Result<graphersal::vector::EmbeddersBuilder, EmbedError>) -> String {
    match result {
        Ok(_) => panic!("the registration should fail"),
        Err(err) => {
            assert!(matches!(err, EmbedError::Registry { .. }), "{err:?}");
            err.to_string()
        }
    }
}

#[test]
fn the_registry_rules() {
    let f32 = VectorElement::F32;
    // Model-style names are accepted.
    for name in [
        "model2vec/potion-base-8M",
        "minishlab/potion-base-8M",
        "acme:v2.1_large",
    ] {
        let registry = Embedders::builder()
            .with(fixed(EmbedderInfo::new(name, 4, f32)))
            .unwrap()
            .default_name(name)
            .unwrap()
            .build();
        assert_eq!(registry.default_embedder().info().name, name);
    }
    // Reserved names.
    for name in ["hash-ngram/1", "hash-ngram/2", "hash-ngram"] {
        let err = registry_error(Embedders::builder().with(fixed(EmbedderInfo::new(name, 4, f32))));
        assert!(err.contains("reserved"), "{err}");
    }
    // A duplicate.
    let once = Embedders::builder()
        .with(fixed(EmbedderInfo::new("acme/a", 4, f32)))
        .unwrap();
    let err = registry_error(once.with(fixed(EmbedderInfo::new("acme/a", 8, f32))));
    assert!(err.contains("already registered"), "{err}");
    // An unknown default.
    let err = registry_error(Embedders::builder().default_name("acme/missing"));
    assert!(err.contains("not registered"), "{err}");
    // Invalid infos.
    for info in [
        EmbedderInfo::new("", 4, f32),
        EmbedderInfo::new("bad name", 4, f32),
        EmbedderInfo::new("x".repeat(129), 4, f32),
        EmbedderInfo::new("acme/zero", 0, f32),
        EmbedderInfo::new("acme/huge", 65_537, f32),
        EmbedderInfo::new("acme/batch", 4, f32).with_max_batch(0),
        EmbedderInfo::new("acme/m", 4, f32).with_model("bad model"),
        EmbedderInfo::new("acme/m", 4, f32).with_model(""),
    ] {
        registry_error(Embedders::builder().with(fixed(info)));
    }
    // A model identity takes @ ? =; by_model finds the first one registered.
    let registry = Embedders::builder()
        .with(fixed(
            EmbedderInfo::new("acme/one", 4, f32).with_model("acme/model@0123456789ab?dim=4"),
        ))
        .unwrap()
        .with(fixed(
            EmbedderInfo::new("acme/two", 4, f32).with_model("acme/model@0123456789ab?dim=4"),
        ))
        .unwrap()
        .build();
    assert_eq!(
        registry
            .by_model("acme/model@0123456789ab?dim=4")
            .map(|e| e.info().name.as_str()),
        Some("acme/one")
    );
    assert_eq!(
        registry
            .by_model("hash-ngram/1")
            .map(|e| e.info().name.as_str()),
        Some("hash-ngram/1")
    );
    assert!(registry.by_model("acme/other").is_none());
    assert_eq!(registry.infos().count(), 3);
}

/// The host example of the book page "Vectors" ("Embedders for hosts"), as written there.
#[test]
fn the_book_host_example_runs() {
    let small = HashNgram::with_dimension("acme/small", 64).unwrap();
    let embedders = Arc::new(
        Embedders::builder()
            .with(Arc::new(small))
            .unwrap()
            .default_name("acme/small")
            .unwrap()
            .build(),
    );
    let graph = GraphSource::tinkerpop_modern();
    let n = graph
        .read()
        .traversal()
        .with_embedders(embedders.clone())
        .v(None::<()>)
        .embed("name", None)
        .count()
        .next()
        .unwrap();
    assert_eq!(n.and_then(|n| n.as_i64()), Some(6));
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_embedders(&mut engine, embedders.clone());
    let mut scope = graphersal::script::graph_scope(Arc::new(graph));
    graphersal::script::set_scope_embedders(&mut scope, embedders);
    let dimension = engine
        .eval_with_scope::<graphersal::script::rhai::INT>(
            &mut scope,
            r#"g.v().embed("name").next().dimension + embed("x").dimension"#,
        )
        .unwrap();
    assert_eq!(dimension, 128);
}

// ---------------------------------------------------------------------------------------------
// A host embedder loaded from a file (the shape of a model-backed embedder)
// ---------------------------------------------------------------------------------------------

/// A host embedder whose shape comes from a small "model file" (`dimension element model`),
/// built fallibly at startup: only public API.
struct FileBacked {
    info: EmbedderInfo,
}

impl FileBacked {
    fn load(name: &str, path: &std::path::Path) -> Result<FileBacked, EmbedError> {
        let text = std::fs::read_to_string(path).map_err(|err| {
            EmbedError::unavailable(format!("cannot read the model {}: {err}", path.display()))
        })?;
        let mut parts = text.split_whitespace();
        let dimension: usize = parts
            .next()
            .and_then(|d| d.parse().ok())
            .ok_or_else(|| EmbedError::unavailable("the model file has no dimension"))?;
        let element = parts
            .next()
            .and_then(VectorElement::from_name)
            .ok_or_else(|| EmbedError::unavailable("the model file has no element kind"))?;
        let model = parts
            .next()
            .ok_or_else(|| EmbedError::unavailable("the model file has no identity"))?;
        Ok(FileBacked {
            info: EmbedderInfo::new(name, dimension, element)
                .with_model(model)
                .with_max_batch(2)
                .with_deterministic(true),
        })
    }
}

impl Embedder for FileBacked {
    fn info(&self) -> &EmbedderInfo {
        &self.info
    }

    fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            ctx.check()?;
            let mut components = vec![0.5f32; self.info.dimension];
            components[0] = text.chars().count() as f32;
            let vector = Vector::from_f32(components)
                .and_then(|v| v.quantize(self.info.element))
                .map_err(|err| EmbedError::failed(err.to_string()))?;
            out.push(vector);
        }
        Ok(out)
    }
}

#[test]
fn a_host_embedder_built_from_a_file_runs_through_the_step_and_the_function() {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "graphersal-embed-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let model = dir.join("model.txt");
    std::fs::write(&model, "5 i8 acme/tiny@00ff00ff00ff").unwrap();
    // A missing file is a startup error the host reports.
    let missing = FileBacked::load("acme/tiny", &dir.join("missing.txt"));
    assert!(matches!(missing, Err(EmbedError::Unavailable { .. })));

    let embedder: Arc<dyn Embedder> = Arc::new(FileBacked::load("acme/tiny", &model).unwrap());
    let registry = Arc::new(
        Embedders::builder()
            .with(embedder)
            .unwrap()
            .default_name("acme/tiny")
            .unwrap()
            .build(),
    );
    let graph = modern();
    let shape = run_ok(
        &graph,
        &registry,
        r#"let v = g.v().has("name", "marko").embed("name").next();
           let q = embed("marko");
           [v.dimension, v.element, q == v, g.v().embed("name").count().next()]"#,
    );
    let shape: Vec<String> = shape
        .into_array()
        .unwrap()
        .into_iter()
        .map(|d| d.to_string())
        .collect();
    assert_eq!(shape, vec!["5", "i8", "true", "6"]);
    // The built-in stays reachable by name.
    let builtin = run_ok(
        &graph,
        &registry,
        r#"embed("marko", #{embedder: "hash-ngram/1"}).dimension"#,
    );
    assert_eq!(builtin.as_int().unwrap(), 256);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// Embedders::scope: the registry of the script entry points that build their own engine
// ---------------------------------------------------------------------------------------------

/// `Embedders::scope` gives `eval_value_with_limits`, `eval_value_atomic` and
/// `eval_value_dry_run` (which build their own engine and scope) the host's registry, for the
/// `embed` step, `embed(text)` and `g.embedders()`; outside the scope the default applies again,
/// and an explicit `set_engine_embedders`/`set_scope_embedders` wins over the scope.
#[test]
fn the_scope_gives_the_script_entry_points_their_registry() {
    use crate::script_host::{eval_value_atomic, eval_value_dry_run, eval_value_with_limits};
    use graphersal::script::ScriptLimits;
    let small = HashNgram::with_dimension("acme/small", 4).unwrap();
    let embedders = Arc::new(
        Embedders::builder()
            .with(Arc::new(small))
            .unwrap()
            .default_name("acme/small")
            .unwrap()
            .build(),
    );
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let limits = ScriptLimits::default();
    let script = r#"[g.v().embed("name").next().dimension, embed("marko").dimension,
                     g.embedders()[0].name]"#;
    let triple = |value: Dynamic| {
        let items = value.into_array().unwrap();
        (
            items[0].as_int().unwrap(),
            items[1].as_int().unwrap(),
            items[2].clone().into_string().unwrap(),
        )
    };
    let expected = (4, 4, "acme/small".to_string());
    embedders.scope(|| {
        let plain = eval_value_with_limits(graph.clone(), script, Default::default(), &limits);
        assert_eq!(triple(plain.unwrap()), expected);
        let atomic = eval_value_atomic(graph.clone(), script, Default::default(), &limits);
        assert_eq!(triple(atomic.unwrap()), expected);
        let (dry, _) =
            eval_value_dry_run(graph.clone(), script, Default::default(), &limits).unwrap();
        assert_eq!(triple(dry), expected);
        // A plain Rust source run in the scope uses it too.
        let vectors = {
            let lock = graph.read();
            lock.traversal()
                .v(None)
                .embed("name", None)
                .to_list()
                .unwrap()
        };
        assert!(vectors[0].as_vector().is_some_and(|v| v.dimension() == 4));
        // An explicit registry wins over the scope.
        let explicit = run_ok(&graph, &Arc::new(Embedders::default()), script);
        assert_eq!(triple(explicit).2, "hash-ngram/1");
    });
    let after = eval_value_with_limits(graph.clone(), script, Default::default(), &limits);
    assert_eq!(triple(after.unwrap()).2, "hash-ngram/1");
    // Scopes nest and restore, also through a panic.
    let inner = Arc::new(Embedders::default());
    embedders.scope(|| {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.scope(|| panic!("inside"))
        }));
        assert!(caught.is_err());
        let value = eval_value_with_limits(graph.clone(), script, Default::default(), &limits);
        assert_eq!(triple(value.unwrap()).2, "acme/small");
    });
}
