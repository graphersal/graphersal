//! The `embed` step, the function `embed(text)`, text queries and the registry with a MOCK
//! embedder attached to a graph like a database's model (crate unit tests: 0.5.0 made the
//! embedder API crate-internal): batching and order, bulk, rollback, wrong answers,
//! cancellation, `g.embedders()`, replay never calling an embedder, the registry rules, and a
//! query text embedded once per execution.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::registry::ModelRegistry;
use super::{EmbedContext, EmbedError, Embedder, EmbedderInfo, Embedders};
use crate::CancelToken;
use crate::graph::Graph;
use crate::script::rhai::{Dynamic, EvalAltResult};
use crate::value::{Vector, VectorElement};
use crate::{GraphSource, TraversalGraph, TraverserError};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Ok,
    Fail,
    WrongCount,
    WrongDimension,
    /// Cancels the token, then reports through `ctx.check()`.
    CancelSelf,
    /// A new value every call (not deterministic).
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

    fn with_model(name: &str, model: &str, max_batch: usize) -> Arc<Mock> {
        Arc::new(Mock {
            info: EmbedderInfo::new(name, 3, VectorElement::F32)
                .with_model(model)
                .with_max_batch(max_batch),
            mode: Mode::Ok,
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

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
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

/// The registry of the built-in plus `mock` (the default when `default`).
fn registry_with(mock: &Arc<Mock>, default: bool) -> Embedders {
    let embedder: Arc<dyn Embedder> = mock.clone();
    let mut builder = Embedders::builder().with(embedder).unwrap();
    if default {
        builder = builder.default_name(&mock.info.name).unwrap();
    }
    builder.build()
}

/// `storage` shared, serving `embedders` as its database's models.
fn serve(storage: TraversalGraph, embedders: Embedders) -> Arc<Graph> {
    let graph = Arc::new(Graph::new(storage));
    assert!(graph.attach_model_registry(ModelRegistry::of_embedders(embedders)));
    graph
}

fn modern_with(mock: &Arc<Mock>) -> Arc<Graph> {
    serve(
        TraversalGraph::tinkerpop_modern(),
        registry_with(mock, false),
    )
}

/// Runs `script` on `graph`, the engine's host graph.
fn run(graph: &Arc<Graph>, script: &str) -> Result<Dynamic, Box<EvalAltResult>> {
    let mut engine = crate::script::engine();
    crate::script::set_engine_graph(&mut engine, graph.clone());
    let mut scope = crate::script::graph_scope(graph.clone());
    engine
        .eval_with_scope::<Dynamic>(&mut scope, script)
        .map_err(|err| crate::script::map_eval_error(err, &Default::default()))
}

fn run_ok(graph: &Arc<Graph>, script: &str) -> Dynamic {
    run(graph, script).unwrap_or_else(|err| panic!("{script}: {err}"))
}

fn run_err(graph: &Arc<Graph>, script: &str) -> String {
    match run(graph, script) {
        Ok(value) => panic!("{script} should fail, gave {value:?}"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn the_step_calls_once_per_batch_and_keeps_the_order() {
    let mock = Mock::new("mock", 4, Mode::Ok);
    let texts: Vec<String> = (0..10)
        .map(|i| format!("text {i}{}", "x".repeat(i)))
        .collect();
    let inject = texts
        .iter()
        .map(|t| format!("{t:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let result = run_ok(
        &modern_with(&mock),
        &format!("g.inject({inject}).embed(#{{embedder: \"mock\"}}).to_list()"),
    );
    assert_eq!(mock.batches(), vec![4, 4, 2]);
    let vectors: Vec<Vector> = result
        .into_array()
        .unwrap()
        .into_iter()
        .map(|v| v.cast::<crate::script::ScriptVector>().vector().clone())
        .collect();
    let expected: Vec<Vector> = texts.iter().map(|t| Mock::vector_of(t)).collect();
    assert_eq!(vectors, expected);
}

#[test]
fn the_step_embeds_each_traverser_once_whatever_its_bulk() {
    let mock = Mock::new("mock", 64, Mode::Ok);
    let count = run_ok(
        &modern_with(&mock),
        r#"g.inject("a", "a", "a", "b").barrier().embed(#{embedder: "mock"}).count().next()"#,
    );
    assert_eq!(count.as_int().unwrap(), 4);
    assert_eq!(mock.batches(), vec![2]);
}

#[test]
fn the_default_model_serves_the_step_and_the_function() {
    let mock = Mock::new("acme/mock", 8, Mode::Ok);
    let graph = serve(
        TraversalGraph::tinkerpop_modern(),
        registry_with(&mock, true),
    );
    let same = run_ok(
        &graph,
        r#"g.v().has("name", "josh").embed("name").next() == embed("josh")"#,
    );
    assert!(same.as_bool().unwrap());
    assert_eq!(mock.calls(), 2);
    // The built-in stays reachable by name.
    let builtin = run_ok(
        &graph,
        r#"embed("marko", #{embedder: "hash-ngram/1"}).dimension"#,
    );
    assert_eq!(builtin.as_int().unwrap(), 256);
}

#[test]
fn a_failing_embedder_rolls_the_traversal_back() {
    let mock = Mock::new("broken", 8, Mode::Fail);
    let graph = modern_with(&mock);
    let err = run_err(
        &graph,
        r#"g.add_v("doc").property("t", "text").property("emb", __.embed("t", #{embedder: "broken"})).iterate()"#,
    );
    assert!(err.contains("the mock model is broken"), "{err}");
    assert_eq!(graph.read().vertex_count(), 6);
}

#[test]
fn wrong_answers_are_bad_output() {
    for mode in [Mode::WrongCount, Mode::WrongDimension] {
        let mock = Mock::new("liar", 8, mode);
        let graph = modern_with(&mock);
        let err = run_err(
            &graph,
            r#"g.inject("a", "b").embed(#{embedder: "liar"}).to_list()"#,
        );
        assert!(err.contains("returned a wrong answer"), "{err}");
        if mode == Mode::WrongDimension {
            let err = run_err(&graph, r#"embed("a", #{embedder: "liar"})"#);
            assert!(err.contains("returned a wrong answer"), "{err}");
        }
    }
}

#[test]
fn a_cancelled_embedding_is_the_execution_cancelled() {
    let mock = Mock::new("cancel", 8, Mode::CancelSelf);
    let graph = modern_with(&mock);
    let token = mock.cancel.clone();
    let result = token.scope(|| {
        graph
            .read()
            .traversal()
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
fn g_embedders_lists_the_models_of_the_graph() {
    let mock = Mock::new("acme/mock", 8, Mode::Ok);
    let script =
        r#"g.embedders().map(|e| e.name + " " + e.model + " " + e.dimension + " " + e.default)"#;
    let names = |value: Dynamic| -> Vec<String> {
        value
            .into_array()
            .unwrap()
            .into_iter()
            .map(|d| d.into_string().unwrap())
            .collect()
    };
    assert_eq!(
        names(run_ok(&modern_with(&mock), script)),
        vec![
            "hash-ngram/1 hash-ngram/1 256 true".to_string(),
            "acme/mock acme/mock 3 false".to_string()
        ]
    );
    // The default model first.
    let graph = serve(
        TraversalGraph::tinkerpop_modern(),
        registry_with(&mock, true),
    );
    assert_eq!(
        names(run_ok(&graph, script))[0],
        "acme/mock acme/mock 3 true"
    );
    // A graph without models: the built-in only.
    let plain = Arc::new(GraphSource::tinkerpop_modern());
    assert_eq!(
        names(run_ok(&plain, script)),
        vec!["hash-ngram/1 hash-ngram/1 256 true".to_string()]
    );
}

#[cfg(feature = "persist")]
#[test]
fn replay_and_recovery_never_call_an_embedder() {
    use crate::persist::{MemDir, Store, StoreOptions};
    let mock = Mock::new("random", 8, Mode::Random);
    let dir = MemDir::new();
    let store = Store::create(&dir, StoreOptions::new()).unwrap();
    let graph = store.graph().clone();
    // The store's registry (of its catalog, no model) serves the mock.
    let serve_mock = |graph: &Arc<Graph>| match graph.model_registry() {
        Some(registry) => registry.serve_for_test(registry_with(&mock, false)),
        None => assert!(
            graph.attach_model_registry(ModelRegistry::of_embedders(registry_with(&mock, false)))
        ),
    };
    serve_mock(&graph);
    let _ = run_ok(
        &graph,
        r#"g.add_v("doc").property("t", "hello").property("emb", __.embed("t", #{embedder: "random"})).iterate()"#,
    );
    let stored = run_ok(&graph, r#"g.v().values("emb").next()"#);
    store.close().unwrap();
    let calls = mock.calls();
    assert_eq!(calls, 1);
    let reopened = Store::open(&dir, StoreOptions::new()).unwrap();
    serve_mock(reopened.graph());
    let again = run_ok(reopened.graph(), r#"g.v().values("emb").next()"#);
    assert_eq!(mock.calls(), calls);
    assert_eq!(
        stored.cast::<crate::script::ScriptVector>(),
        again.cast::<crate::script::ScriptVector>()
    );
}

#[test]
fn the_registry_rules() {
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
    fn refused<T>(result: Result<T, String>) -> String {
        match result {
            Ok(_) => panic!("the registration should fail"),
            Err(err) => err,
        }
    }
    let f32 = VectorElement::F32;
    // Repository names (the names of database models) are accepted.
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
        let err = refused(Embedders::builder().with(fixed(EmbedderInfo::new(name, 4, f32))));
        assert!(err.contains("reserved"), "{err}");
    }
    // A duplicate.
    let once = Embedders::builder()
        .with(fixed(EmbedderInfo::new("acme/a", 4, f32)))
        .unwrap();
    let err = refused(once.with(fixed(EmbedderInfo::new("acme/a", 8, f32))));
    assert!(err.contains("already registered"), "{err}");
    // An unknown default.
    let err = refused(Embedders::builder().default_name("acme/missing"));
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
        refused(Embedders::builder().with(fixed(info)));
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

/// A text query embeds its text once per execution (moved here from
/// `tests/all/text_query_tests.rs`: counting calls needs a mock).
#[test]
fn the_query_text_is_embedded_once_per_execution() {
    let mock = Mock::with_model("mock", "mock@1", 2);
    let mut storage = TraversalGraph::new();
    for i in 0..6 {
        storage
            .traversal_mut()
            .add_v("doc")
            .property("text", format!("text {i}"))
            .iterate()
            .unwrap();
    }
    {
        let mut g = storage.traversal_mut();
        let schema = g
            .infer_schema()
            .unwrap()
            .with_mode(crate::graph::schema::SchemaMode::Open);
        g.set_schema(schema).unwrap();
    }
    let graph = serve(storage, registry_with(&mock, true));
    let _ = run_ok(
        &graph,
        r#"g.v().property("emb", __.embed("text")).iterate();
           g.patch_schema(#{vertices: #{doc: #{schema: #{properties: #{emb: #{type: "array", format: "vector", dimension: 3, embedder: "mock@1"}}}}}})"#,
    );
    let before = mock.calls();
    let _ = run_ok(&graph, r#"g.v().similarity("emb", "text 1").to_list()"#);
    assert_eq!(mock.calls(), before + 1);
    let _ = run_ok(&graph, r#"g.v().nearest("emb", "text 1", 2).to_list()"#);
    assert_eq!(mock.calls(), before + 2);
    assert_eq!(
        mock.seen.lock().unwrap().last().unwrap(),
        &vec!["text 1".to_string()]
    );
}
