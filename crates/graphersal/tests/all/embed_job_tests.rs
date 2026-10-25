//! The job `embed_property` and vector provenance (book page "Vectors"): pages and calls, the
//! embedder running without the graph's lock (a concurrent writer commits while it runs),
//! texts changed or elements removed between the phases, a failure on a page and the rerun,
//! cancellation, the job inside a transaction and a whole-script unit, schema refusals, the
//! schema keyword `embedder` (parse, print, placement, diff, inference), the text queries of
//! `nearest`/`similarity`, and the same results on the `Forwarding` storage.

#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use graphersal::changes::{ChangeSet, CommitHook, Mutation};
use graphersal::exec::CancelToken;
use graphersal::prelude::*;
use graphersal::schema::SchemaChangeKind;
use graphersal::script::rhai::{Dynamic, EvalAltResult};
use graphersal::storage::{ElementKind, GraphStorage, IntoAnyGraph};
use graphersal::vector::{
    EmbedContext, EmbedError, EmbedJob, EmbedReport, Embedder, EmbedderInfo, Embedders, Vector,
    VectorElement,
};

use crate::forwarding::Forwarding;
use crate::script_host::{ScriptLimits, eval_value_atomic};

// ---------------------------------------------------------------------------------------------
// A scriptable mock embedder
// ---------------------------------------------------------------------------------------------

type Hook = Box<dyn Fn(usize, &[&str]) -> Result<(), EmbedError> + Send + Sync>;

struct Mock {
    info: EmbedderInfo,
    calls: AtomicUsize,
    seen: Mutex<Vec<Vec<String>>>,
    /// Runs inside `embed` before the answer (call number, texts).
    hook: Mutex<Option<Hook>>,
}

impl Mock {
    fn new(name: &str, max_batch: usize) -> Arc<Mock> {
        Arc::new(Mock {
            info: EmbedderInfo::new(name, 3, VectorElement::F32)
                .with_model(format!("{name}@1"))
                .with_max_batch(max_batch),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            hook: Mutex::new(None),
        })
    }

    fn on_embed(
        &self,
        hook: impl Fn(usize, &[&str]) -> Result<(), EmbedError> + Send + Sync + 'static,
    ) {
        *self.hook.lock().unwrap() = Some(Box::new(hook));
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn vector_of(text: &str) -> Vector {
        let sum: u32 = text.chars().map(|c| c as u32).sum();
        Vector::from_f32(vec![text.chars().count() as f32, (sum % 1000) as f32, 1.0]).unwrap()
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
        if let Some(hook) = self.hook.lock().unwrap().as_ref() {
            hook(call, texts)?;
        }
        ctx.check()?;
        Ok(texts.iter().map(|t| Mock::vector_of(t)).collect())
    }
}

fn registry_with(mock: &Arc<Mock>) -> Embedders {
    let embedder: Arc<dyn Embedder> = mock.clone();
    Embedders::builder()
        .with(embedder)
        .unwrap()
        .default_name(&mock.info.name)
        .unwrap()
        .build()
}

/// `n` vertices `doc` with `text` "text 0", "text 1", ... and an open schema of that.
fn docs(n: usize) -> Arc<Graph> {
    let graph = Arc::new(GraphSource::empty());
    {
        let mut lock = graph.write();
        for i in 0..n {
            lock.traversal_mut()
                .add_v("doc")
                .property("text", format!("text {i}"))
                .iterate()
                .unwrap();
        }
        let mut g = lock.traversal_mut();
        let schema = g.infer_schema().unwrap().with_mode(SchemaMode::Open);
        g.set_schema(schema).unwrap();
    }
    graph
}

fn run_job(graph: &Graph, job: &EmbedJob, embedders: &Embedders) -> Result<EmbedReport, String> {
    graph
        .embed_property(job, embedders, &CancelToken::new(), &mut |_| {})
        .map_err(|err| err.error.to_string())
}

/// How many vertices have the property `key`.
fn with_property(graph: &Graph, key: &str) -> usize {
    let lock = graph.read();
    lock.traversal()
        .v(None)
        .has_key(key)
        .to_list()
        .unwrap()
        .len()
}

fn declared_embedder(graph: &Graph, label: &str, key: &str) -> Option<String> {
    let schema = graph.read().get_schema()?;
    schema
        .vertices
        .get(label)?
        .schema
        .properties
        .get(key)?
        .vector
        .as_ref()?
        .embedder
        .clone()
}

/// `script` on `graph` with `embedders` given to the engine and the scope.
fn script(
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

fn script_err(graph: &Arc<Graph>, embedders: &Arc<Embedders>, text: &str) -> String {
    match script(graph, embedders, text) {
        Ok(value) => panic!("{text} should fail, gave {value:?}"),
        Err(err) => err.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// The job
// ---------------------------------------------------------------------------------------------

#[test]
fn the_job_embeds_page_by_page_and_declares_the_model() {
    let graph = docs(10);
    let mock = Mock::new("mock", 3);
    let embedders = registry_with(&mock);
    let mut pages = Vec::new();
    let report = graph
        .embed_property(
            &EmbedJob::new("doc", "text", "emb"),
            &embedders,
            &CancelToken::new(),
            &mut |report| pages.push(report.embedded),
        )
        .unwrap();
    assert_eq!(report.embedded, 10);
    assert_eq!(report.units, 4);
    assert!(report.schema_changed);
    assert_eq!(mock.calls(), 4);
    assert_eq!(pages, vec![3, 6, 9, 10]);
    assert_eq!(with_property(&graph, "emb"), 10);
    assert_eq!(
        declared_embedder(&graph, "doc", "emb").as_deref(),
        Some("mock@1")
    );
    // The declaration has the embedder's shape.
    let schema = graph.read().get_schema().unwrap();
    let vector = schema.vertices["doc"].schema.properties["emb"]
        .vector
        .clone()
        .unwrap();
    assert_eq!(
        (vector.element, vector.dimension),
        (VectorElement::F32, Some(3))
    );
    // The vectors are the embedder's.
    let lock = graph.read();
    let stored = lock
        .traversal()
        .v(None)
        .has("text", "text 4")
        .values("emb")
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(stored.as_vector(), Some(&Mock::vector_of("text 4")));
}

#[test]
fn a_batch_option_sets_the_page_size() {
    let graph = docs(5);
    let mock = Mock::new("mock", 64);
    let report = run_job(
        &graph,
        &EmbedJob::new("doc", "text", "emb").batch(2),
        &registry_with(&mock),
    )
    .unwrap();
    assert_eq!((report.embedded, report.units, mock.calls()), (5, 3, 3));
}

#[test]
fn the_embedder_runs_without_the_graph_lock() {
    let graph = docs(4);
    let mock = Mock::new("mock", 2);
    let writer = graph.clone();
    let committed = Arc::new(AtomicBool::new(false));
    let flag = committed.clone();
    mock.on_embed(move |call, _| {
        if call == 0 {
            // Another thread writes while the embedder runs: it gets the write lock and
            // commits, which it could not if the job held any lock of the graph now.
            let graph = writer.clone();
            let (done, wait) = mpsc::channel();
            std::thread::spawn(move || {
                graph
                    .write()
                    .traversal_mut()
                    .add_v("other")
                    .property("name", "concurrent")
                    .iterate()
                    .unwrap();
                done.send(()).unwrap();
            });
            if wait.recv_timeout(Duration::from_secs(10)).is_ok() {
                flag.store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    });
    let report = run_job(
        &graph,
        &EmbedJob::new("doc", "text", "emb"),
        &registry_with(&mock),
    )
    .unwrap();
    assert!(committed.load(Ordering::SeqCst), "the writer was blocked");
    assert_eq!(report.embedded, 4);
    assert_eq!(
        graph
            .read()
            .traversal()
            .v(None)
            .has_label("other")
            .count()
            .next()
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(1)
    );
}

#[test]
fn texts_changed_or_removed_between_the_phases_are_skipped() {
    let graph = docs(4);
    let mock = Mock::new("mock", 4);
    let writer = graph.clone();
    mock.on_embed(move |_, _| {
        // No lock is held here: change one text, remove another element.
        let mut lock = writer.write();
        lock.traversal_mut()
            .v(None)
            .has("text", "text 1")
            .property("text", "edited")
            .iterate()
            .unwrap();
        lock.traversal_mut()
            .v(None)
            .has("text", "text 2")
            .drop()
            .iterate()
            .unwrap();
        Ok(())
    });
    let report = run_job(
        &graph,
        &EmbedJob::new("doc", "text", "emb"),
        &registry_with(&mock),
    )
    .unwrap();
    assert_eq!(
        (
            report.embedded,
            report.skipped_changed,
            report.skipped_removed
        ),
        (2, 1, 1)
    );
    assert_eq!(with_property(&graph, "emb"), 2);
}

#[test]
fn absent_featureless_and_non_text_values_are_counted() {
    let graph = Arc::new(GraphSource::empty());
    {
        let mut lock = graph.write();
        for text in ["graph", "", "!!!", "indexes"] {
            lock.traversal_mut()
                .add_v("doc")
                .property("text", text)
                .iterate()
                .unwrap();
        }
        lock.traversal_mut()
            .add_v("doc")
            .property("text", 42)
            .iterate()
            .unwrap();
        lock.traversal_mut().add_v("doc").iterate().unwrap();
        let mut g = lock.traversal_mut();
        let schema = g.infer_schema().unwrap().with_mode(SchemaMode::Open);
        g.set_schema(schema).unwrap();
    }
    let report = run_job(
        &graph,
        &EmbedJob::new("doc", "text", "emb"),
        &Embedders::default(),
    )
    .unwrap();
    assert_eq!(
        (report.embedded, report.skipped_no_text, report.not_text),
        (2, 3, 1)
    );
}

#[test]
fn a_failure_keeps_the_earlier_pages_and_a_rerun_completes() {
    let graph = docs(7);
    let mock = Mock::new("mock", 3);
    mock.on_embed(|call, _| {
        if call == 1 {
            Err(EmbedError::failed("the model went away"))
        } else {
            Ok(())
        }
    });
    let embedders = registry_with(&mock);
    let failed = graph
        .embed_property(
            &EmbedJob::new("doc", "text", "emb"),
            &embedders,
            &CancelToken::new(),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(
        failed.error.to_string().contains("the model went away"),
        "{}",
        failed.error
    );
    assert_eq!((failed.report.embedded, failed.report.units), (3, 1));
    assert_eq!(with_property(&graph, "emb"), 3);
    // The rerun continues where the job stopped (only_missing).
    let report = run_job(&graph, &EmbedJob::new("doc", "text", "emb"), &embedders).unwrap();
    assert_eq!(
        (report.embedded, report.units, report.schema_changed),
        (4, 2, false)
    );
    assert_eq!(with_property(&graph, "emb"), 7);
    // only_missing: false embeds everything again.
    let again = run_job(
        &graph,
        &EmbedJob::new("doc", "text", "emb").only_missing(false),
        &embedders,
    )
    .unwrap();
    assert_eq!(again.embedded, 7);
}

#[test]
fn cancel_stops_the_job_between_pages_and_inside_the_embedder() {
    let graph = docs(6);
    let mock = Mock::new("mock", 2);
    let embedders = registry_with(&mock);
    let cancel = CancelToken::new();
    let failed = graph
        .embed_property(
            &EmbedJob::new("doc", "text", "emb"),
            &embedders,
            &cancel,
            &mut |_| cancel.cancel(),
        )
        .unwrap_err();
    assert!(
        failed.error.to_string().contains("cancelled"),
        "{}",
        failed.error
    );
    assert_eq!((failed.report.embedded, failed.report.units), (2, 1));
    assert_eq!(mock.calls(), 1);
    // Cancelled inside the embedder (ctx.check()): the page is not written.
    let token = CancelToken::new();
    let inner = token.clone();
    mock.on_embed(move |_, _| {
        inner.cancel();
        Ok(())
    });
    let failed = graph
        .embed_property(
            &EmbedJob::new("doc", "text", "emb"),
            &embedders,
            &token,
            &mut |_| {},
        )
        .unwrap_err();
    assert!(
        failed.error.to_string().contains("cancelled"),
        "{}",
        failed.error
    );
    assert_eq!(failed.report.embedded, 0);
    assert_eq!(with_property(&graph, "emb"), 2);
}

#[test]
fn inside_a_transaction_the_job_is_part_of_it() {
    let graph = docs(5);
    let mock = Mock::new("mock", 2);
    let embedders = registry_with(&mock);
    let job = EmbedJob::new("doc", "text", "emb");
    // Rolled back: nothing is left, neither the vectors nor the declaration.
    let outcome: Result<(), GraphError> = graph.write().transaction(|storage| {
        let report = job
            .run_in(storage, &embedders, &CancelToken::new(), &mut |_| {})
            .unwrap();
        assert_eq!((report.embedded, report.units), (5, 3));
        Err(GraphError::unsupported("a veto of the test"))
    });
    assert!(outcome.is_err());
    assert_eq!(with_property(&graph, "emb"), 0);
    assert_eq!(declared_embedder(&graph, "doc", "emb"), None);
    // Committed: everything at once, in one commit.
    let before = graph.read().last_commit_seq();
    graph
        .write()
        .transaction(|storage| {
            job.run_in(storage, &embedders, &CancelToken::new(), &mut |_| {})
                .map(|_| ())
                .map_err(|_| GraphError::unsupported("the job"))
        })
        .unwrap();
    assert_eq!(graph.read().last_commit_seq(), before + 1);
    assert_eq!(with_property(&graph, "emb"), 5);
    assert_eq!(
        declared_embedder(&graph, "doc", "emb").as_deref(),
        Some("mock@1")
    );
}

#[test]
fn in_a_whole_script_unit_the_job_rolls_back_with_the_script() {
    let graph = docs(3);
    let limits = ScriptLimits::default();
    let failed = eval_value_atomic(
        graph.clone(),
        r#"g.embed_property(#{label: "doc", from: "text", to: "emb"}); throw "stop""#,
        HashMap::new(),
        &limits,
    );
    assert!(failed.is_err());
    assert_eq!(with_property(&graph, "emb"), 0);
    let report = eval_value_atomic(
        graph.clone(),
        r#"g.embed_property(#{label: "doc", from: "text", to: "emb", batch: 1})"#,
        HashMap::new(),
        &limits,
    )
    .unwrap();
    let report = report.cast::<graphersal::script::rhai::Map>();
    assert_eq!(report["embedded"].as_int().unwrap(), 3);
    assert_eq!(report["units"].as_int().unwrap(), 3);
    assert_eq!(with_property(&graph, "emb"), 3);
    assert_eq!(
        declared_embedder(&graph, "doc", "emb").as_deref(),
        Some("hash-ngram/1")
    );
}

/// Records the mutation kinds of every commit.
struct Recorder(Arc<Mutex<Vec<Vec<&'static str>>>>);

impl CommitHook for Recorder {
    fn name(&self) -> &str {
        "recorder"
    }
    fn after_commit(&mut self, changes: &ChangeSet) {
        let kinds = changes
            .mutations()
            .iter()
            .map(|m| match m {
                Mutation::SetSchema { .. } => "schema",
                Mutation::SetProperty { .. } => "property",
                _ => "other",
            })
            .collect();
        self.0.lock().unwrap().push(kinds);
    }
}

#[test]
fn the_declaration_is_in_the_unit_of_the_first_page() {
    let graph = docs(3);
    let commits = Arc::new(Mutex::new(Vec::new()));
    graph
        .write()
        .register_commit_hook(Box::new(Recorder(commits.clone())))
        .unwrap();
    let mock = Mock::new("mock", 2);
    run_job(
        &graph,
        &EmbedJob::new("doc", "text", "emb"),
        &registry_with(&mock),
    )
    .unwrap();
    assert_eq!(
        *commits.lock().unwrap(),
        vec![vec!["schema", "property", "property"], vec!["property"]]
    );
}

#[test]
fn the_job_needs_an_attached_schema() {
    for setup in [None, Some(SchemaMode::None)] {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        if let Some(mode) = setup {
            let mut lock = graph.write();
            let mut g = lock.traversal_mut();
            let schema = g.infer_schema().unwrap().with_mode(mode);
            g.set_schema(schema).unwrap();
        }
        let mock = Mock::new("mock", 2);
        let failed = graph
            .embed_property(
                &EmbedJob::new("person", "name", "emb"),
                &registry_with(&mock),
                &CancelToken::new(),
                &mut |_| {},
            )
            .unwrap_err();
        assert!(
            failed.error.to_string().contains("needs a schema attached"),
            "{}",
            failed.error
        );
        let help = failed.help().unwrap();
        assert!(
            help.contains("g.set_schema(g.infer_schema(), SchemaMode.open)"),
            "{help}"
        );
        assert_eq!(mock.calls(), 0);
        assert_eq!(with_property(&graph, "emb"), 0);
    }
}

#[test]
fn a_conflicting_declaration_stops_the_job_before_the_embedder_runs() {
    let graph = docs(2);
    let mock = Mock::new("mock", 2);
    let embedders = registry_with(&mock);
    let patch = |node: &str| {
        let declared = schema_with(node).unwrap().vertices["doc"].schema.properties["emb"].clone();
        let mut lock = graph.write();
        let mut schema = lock.get_schema().unwrap();
        schema.vertices["doc"]
            .schema
            .properties
            .insert("emb".to_string(), declared);
        lock.traversal_mut().set_schema(schema).unwrap();
    };
    // Declared as a string: the schema's type mismatch.
    patch(r#"{"type": "string"}"#);
    let err = run_job(&graph, &EmbedJob::new("doc", "text", "emb"), &embedders).unwrap_err();
    assert!(
        err.contains("Type mismatch on vertex 'doc.emb': expected string"),
        "{err}"
    );
    // Another dimension.
    patch(r#"{"type": "array", "format": "vector", "dimension": 8}"#);
    let err = run_job(&graph, &EmbedJob::new("doc", "text", "emb"), &embedders).unwrap_err();
    assert!(err.contains("expected vector<f32, 8>"), "{err}");
    // Another model.
    patch(r#"{"type": "array", "format": "vector", "dimension": 3, "embedder": "acme/other@2"}"#);
    let err = run_job(&graph, &EmbedJob::new("doc", "text", "emb"), &embedders).unwrap_err();
    assert!(
        err.contains(r#"vectors of the model "acme/other@2""#),
        "{err}"
    );
    assert!(err.contains(r#"has the model "mock@1""#), "{err}");
    assert_eq!(mock.calls(), 0);
    // A matching declaration without embedder gets it added.
    patch(r#"{"type": "array", "format": "vector", "dimension": 3}"#);
    let report = run_job(&graph, &EmbedJob::new("doc", "text", "emb"), &embedders).unwrap();
    assert!(report.schema_changed);
    assert_eq!(
        declared_embedder(&graph, "doc", "emb").as_deref(),
        Some("mock@1")
    );
}

#[test]
fn existing_values_violating_the_declaration_stop_the_job_with_its_report() {
    let graph = docs(3);
    graph
        .write()
        .traversal_mut()
        .v(None)
        .has("text", "text 0")
        .property("emb", "not a vector")
        .iterate()
        .unwrap();
    let mock = Mock::new("mock", 2);
    let failed = graph
        .embed_property(
            &EmbedJob::new("doc", "text", "emb").only_missing(false),
            &registry_with(&mock),
            &CancelToken::new(),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(
        failed.error.to_string().contains("violate"),
        "{}",
        failed.error
    );
    assert_eq!((failed.report.embedded, failed.report.units), (0, 0));
    assert_eq!(declared_embedder(&graph, "doc", "emb"), None);
}

#[test]
fn edges_and_jpath_sources() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    {
        let mut lock = graph.write();
        lock.traversal_mut()
            .e(None)
            .has_label("knows")
            .property(
                "note",
                ElementProperty::from_entries([(
                    ElementProperty::from("text"),
                    ElementProperty::from("old friends"),
                )]),
            )
            .iterate()
            .unwrap();
        let mut g = lock.traversal_mut();
        let schema = g.infer_schema().unwrap().with_mode(SchemaMode::Open);
        g.set_schema(schema).unwrap();
    }
    let job = EmbedJob::new("knows", "note.text".parse::<JsonPath>().unwrap(), "emb")
        .element(ElementKind::Edge);
    let report = run_job(&graph, &job, &Embedders::default()).unwrap();
    assert_eq!(report.embedded, 2);
    let schema = graph.read().get_schema().unwrap();
    let declared = schema.edges["knows"].schema.properties["emb"]
        .vector
        .clone()
        .unwrap();
    assert_eq!(declared.embedder.as_deref(), Some("hash-ngram/1"));
}

#[test]
fn forwarding_gives_the_same_result() {
    fn state<S: GraphStorage + Default + Send + Sync + 'static>(graph: &Arc<Graph<S>>) -> String {
        graphersal::script::eval_value_with_limits(
            graph.clone().into_any_graph(),
            r#"[g.V().hasLabel("doc").order().by("text").values("emb").toList(), g.get_schema()].to_string()"#,
            HashMap::new(),
            &ScriptLimits::default(),
            Arc::new(graphersal::auth::AllowAll),
        )
        .unwrap()
        .to_string()
    }
    let reference = docs(5);
    let forwarded = {
        let inner = Arc::try_unwrap(docs(5)).ok().unwrap().into_inner();
        Arc::new(Graph::new(Forwarding::new(inner)))
    };
    let mock = Mock::new("mock", 2);
    let embedders = registry_with(&mock);
    let job = EmbedJob::new("doc", "text", "emb");
    let a = reference
        .embed_property(&job, &embedders, &CancelToken::new(), &mut |_| {})
        .unwrap();
    let b = forwarded
        .embed_property(&job, &embedders, &CancelToken::new(), &mut |_| {})
        .unwrap();
    assert_eq!(
        (a.embedded, a.units, a.schema_changed),
        (b.embedded, b.units, b.schema_changed)
    );
    assert_eq!(state(&reference), state(&forwarded));
}

// ---------------------------------------------------------------------------------------------
// The DSL
// ---------------------------------------------------------------------------------------------

#[test]
fn the_dsl_job_returns_the_report_map() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let embedders = Arc::new(Embedders::default());
    let report = script(
        &graph,
        &embedders,
        r#"g.set_schema(g.infer_schema(), SchemaMode.open);
           g.embed_property(#{label: "person", from: "name", to: "emb", batch: 3})"#,
    )
    .unwrap()
    .cast::<graphersal::script::rhai::Map>();
    let int = |key: &str| report[key].as_int().unwrap();
    assert_eq!((int("embedded"), int("units")), (4, 2));
    assert_eq!(
        report["embedder"].clone().into_string().unwrap(),
        "hash-ngram/1"
    );
    assert_eq!(
        report["model"].clone().into_string().unwrap(),
        "hash-ngram/1"
    );
    assert!(report["schema_changed"].as_bool().unwrap());
    for (options, expected) in [
        (
            r#"#{label: "person", from: "name"}"#,
            "the keys label, from and to",
        ),
        (
            r#"#{label: "person", from: "name", to: "e", size: 3}"#,
            r#"the key "size""#,
        ),
        (
            r#"#{label: "person", from: "name", to: "e", batch: 0}"#,
            "batch: a number of elements >= 1",
        ),
        (
            r#"#{label: "person", from: "name", to: "e", element: "node"}"#,
            r#"element: "vertex" or "edge""#,
        ),
        (
            r#"#{label: "person", from: 1, to: "e"}"#,
            "from: a property key",
        ),
        (
            r#"#{label: "person", from: "name", to: "e", embedder: "nope"}"#,
            "there is no embedder",
        ),
    ] {
        let err = script_err(&graph, &embedders, &format!("g.embed_property({options})"));
        assert!(err.contains(expected), "{options}: {err}");
    }
}

// ---------------------------------------------------------------------------------------------
// Text queries
// ---------------------------------------------------------------------------------------------

fn embedded_modern() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = script(
        &graph,
        &Arc::new(Embedders::default()),
        r#"g.set_schema(g.infer_schema(), SchemaMode.open);
           g.embed_property(#{label: "person", from: "name", to: "emb"});
           g.embed_property(#{label: "software", from: "name", to: "emb"})"#,
    )
    .unwrap();
    graph
}

#[test]
fn a_text_query_equals_the_query_with_its_embedded_vector() {
    let graph = embedded_modern();
    let embedders = Arc::new(Embedders::default());
    for (text, vector) in [
        (
            r#"g.v().nearest("emb", "marco", 3).values("name").to_list()"#,
            r#"g.v().nearest("emb", embed("marco"), 3).values("name").to_list()"#,
        ),
        (
            r#"g.v().similarity("emb", "ripple").to_list()"#,
            r#"g.v().similarity("emb", embed("ripple")).to_list()"#,
        ),
        (
            r#"g.v().similarity("emb", "ripple", "l2").to_list()"#,
            r#"g.v().similarity("emb", embed("ripple"), "l2").to_list()"#,
        ),
        (
            r#"g.v().nearest("emb", "lop", 1, #{metric: "dot"}).values("name").to_list()"#,
            r#"g.v().nearest("emb", embed("lop"), 1, #{metric: "dot"}).values("name").to_list()"#,
        ),
    ] {
        let a = script(&graph, &embedders, text).unwrap().to_string();
        let b = script(&graph, &embedders, vector).unwrap().to_string();
        assert_eq!(a, b, "{text}");
    }
    let best = script(
        &graph,
        &embedders,
        r#"g.v().nearest("emb", "marco", 1).values("name").next()"#,
    )
    .unwrap()
    .into_string()
    .unwrap();
    assert_eq!(best, "marko");
}

#[test]
fn the_query_text_is_embedded_once_per_execution() {
    let graph = docs(6);
    let mock = Mock::new("mock", 2);
    let embedders = Arc::new(registry_with(&mock));
    run_job(&graph, &EmbedJob::new("doc", "text", "emb"), &embedders).unwrap();
    let before = mock.calls();
    let _ = script(
        &graph,
        &embedders,
        r#"g.v().similarity("emb", "text 1").to_list()"#,
    )
    .unwrap();
    assert_eq!(mock.calls(), before + 1);
    let _ = script(
        &graph,
        &embedders,
        r#"g.v().nearest("emb", "text 1", 2).to_list()"#,
    )
    .unwrap();
    assert_eq!(mock.calls(), before + 2);
    assert_eq!(
        mock.seen.lock().unwrap().last().unwrap(),
        &vec!["text 1".to_string()]
    );
}

#[test]
fn the_profile_names_the_resolved_embedder() {
    let graph = embedded_modern();
    let embedders = Arc::new(Embedders::default());
    let profile = script(
        &graph,
        &embedders,
        r#"g.v().nearest("emb", "marco", 2).similarity("emb", "marco", "l2").profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(
        profile.contains(r#"nearest("emb", "marco", 2, embedder: "hash-ngram/1")"#),
        "{profile}"
    );
    assert!(
        profile.contains(r#"similarity("emb", "marco", "l2", embedder: "hash-ngram/1")"#),
        "{profile}"
    );
}

#[test]
fn text_query_refusals() {
    let embedders = Arc::new(Embedders::default());
    // No schema; mode none.
    let modern = Arc::new(GraphSource::tinkerpop_modern());
    let err = script_err(
        &modern,
        &embedders,
        r#"g.v().nearest("emb", "marco", 2).to_list()"#,
    );
    assert!(err.contains("nearest needs a schema attached"), "{err}");
    // No embedder declared.
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = script(
        &graph,
        &embedders,
        r#"g.set_schema(g.infer_schema(), SchemaMode.open)"#,
    )
    .unwrap();
    let err = script_err(
        &graph,
        &embedders,
        r#"g.v().similarity("emb", "marco").to_list()"#,
    );
    assert!(
        err.contains(r#"declares no embedder for the vector property "emb""#),
        "{err}"
    );
    // A jpath key or the value form.
    let graph = embedded_modern();
    let err = script_err(
        &graph,
        &embedders,
        r#"g.v().similarity(jpath("emb"), "marco").to_list()"#,
    );
    assert!(err.contains("needs a top-level property name"), "{err}");
    let err = script_err(
        &graph,
        &embedders,
        r#"g.v().values("emb").similarity("marco").to_list()"#,
    );
    assert!(err.contains("needs a top-level property name"), "{err}");
    // A model nobody registered.
    let _ = script(
        &graph,
        &embedders,
        r#"g.patch_schema(#{vertices: #{software: #{schema: #{properties: #{emb: #{embedder: "acme/other@2"}}}}}})"#,
    )
    .unwrap();
    let err = script_err(
        &graph,
        &embedders,
        r#"g.v().has_label("software").nearest("emb", "lop", 1).to_list()"#,
    );
    assert!(
        err.contains(r#"no registered embedder has the model "acme/other@2""#),
        "{err}"
    );
    // Two models met.
    let err = script_err(
        &graph,
        &embedders,
        r#"g.v().nearest("emb", "lop", 1).to_list()"#,
    );
    assert!(err.contains("one text query cannot search both"), "{err}");
    // Only labels of one model: fine.
    let best = script(
        &graph,
        &embedders,
        r#"g.v().has_label("person").nearest("emb", "josh", 1).values("name").next()"#,
    )
    .unwrap()
    .into_string()
    .unwrap();
    assert_eq!(best, "josh");
    // Every candidate unregistered: refused before the run.
    let graph = embedded_modern();
    let _ = script(
        &graph,
        &embedders,
        r#"g.patch_schema(#{vertices: #{software: #{schema: #{properties: #{emb: #{embedder: "x/1"}}}}, person: #{schema: #{properties: #{emb: #{embedder: "x/1"}}}}}})"#,
    )
    .unwrap();
    let err = script_err(
        &graph,
        &embedders,
        r#"g.v().has_label("nothing").nearest("emb", "lop", 1).to_list()"#,
    );
    assert!(
        err.contains(r#"no registered embedder has the model "x/1""#),
        "{err}"
    );
}

#[test]
fn vector_queries_keep_working_without_a_schema() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let embedders = Arc::new(Embedders::default());
    let best = script(
        &graph,
        &embedders,
        r#"g.v().has_label("person").property("emb", __.embed("name")).iterate();
           g.v().nearest("emb", embed("vadas"), 1).values("name").next()"#,
    )
    .unwrap()
    .into_string()
    .unwrap();
    assert_eq!(best, "vadas");
}

#[test]
fn a_plain_write_checks_only_element_and_dimension() {
    let graph = embedded_modern();
    let embedders = Arc::new(Embedders::default());
    // A vector of the declared shape from anywhere is stored, whatever its provenance.
    let _ = script(
        &graph,
        &embedders,
        r#"g.v().has("name", "marko").property("emb", embed("anything")).iterate()"#,
    )
    .unwrap();
    let err = script_err(
        &graph,
        &embedders,
        r#"g.v().has("name", "marko").property("emb", vector([1.0, 2.0])).iterate()"#,
    );
    assert!(err.contains("vector<f32, 256>"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// The schema keyword `embedder`
// ---------------------------------------------------------------------------------------------

fn schema_with(node: &str) -> Result<GraphSchema, String> {
    GraphSchema::from_json(&format!(
        r#"{{"mode": "open", "vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{"emb": {node}}}}}}}}}}}"#
    ))
    .map_err(|err| err.to_string())
}

#[test]
fn the_embedder_keyword_parses_prints_and_round_trips() {
    let schema = schema_with(
        r#"{"type": "array", "format": "vector", "dimension": 256, "embedder": "hash-ngram/1", "metric": "cosine"}"#,
    )
    .unwrap();
    let vector = schema.vertices["doc"].schema.properties["emb"]
        .vector
        .clone()
        .unwrap();
    assert_eq!(vector.embedder.as_deref(), Some("hash-ngram/1"));
    let text = schema.to_json();
    assert!(
        text.contains(r#""metric":"cosine","embedder":"hash-ngram/1""#),
        "canonical order: {text}"
    );
    assert_eq!(GraphSchema::from_json(&text).unwrap(), schema);
    let err = schema_with(r#"{"type": "array", "embedder": "hash-ngram/1"}"#).unwrap_err();
    assert!(
        err.contains("embedder needs \"format\": \"vector\""),
        "{err}"
    );
    let err =
        schema_with(r#"{"type": "array", "format": "vector", "embedder": "a b"}"#).unwrap_err();
    assert!(err.contains("model identity"), "{err}");
    let err = schema_with(r#"{"type": "array", "format": "vector", "embedder": 3}"#).unwrap_err();
    assert!(err.contains("embedder"), "{err}");
    let ok = schema_with(
        r#"{"type": "array", "format": "vector", "embedder": "model2vec/minishlab/potion-base-8M@3f9a1c0d2b7e"}"#,
    );
    assert!(ok.is_ok(), "{ok:?}");
}

#[test]
fn the_embedder_keyword_sits_on_top_level_properties_only() {
    let graph = Arc::new(GraphSource::empty());
    let nested = GraphSchema::from_json(
        r#"{"mode": "open", "vertices": {"doc": {"schema": {"type": "object", "properties": {"meta": {"type": "object", "properties": {"emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1"}}}}}}}}"#,
    )
    .unwrap();
    let err = graph
        .write()
        .traversal_mut()
        .set_schema(nested)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("embedder applies only to a label's top-level vector properties"),
        "{err}"
    );
    assert!(
        err.contains("/vertices/doc/schema/properties/meta/properties/emb/embedder"),
        "{err}"
    );
    let top = schema_with(r#"{"type": "array", "format": "vector", "embedder": "hash-ngram/1"}"#)
        .unwrap();
    graph.write().traversal_mut().set_schema(top).unwrap();
}

#[test]
fn an_embedder_change_is_a_diff_entry() {
    let diff = |a: &str, b: &str| {
        graphersal::schema::SchemaDiff::between(
            Some(&schema_with(a).unwrap()),
            &schema_with(b).unwrap(),
        )
        .changes
        .into_iter()
        .filter(|c| c.kind == SchemaChangeKind::EmbedderChanged)
        .map(|c| (c.before, c.after, c.compatible, c.kind.as_str()))
        .collect::<Vec<_>>()
    };
    let plain = r#"{"type": "array", "format": "vector"}"#;
    let one = r#"{"type": "array", "format": "vector", "embedder": "a/1"}"#;
    let two = r#"{"type": "array", "format": "vector", "embedder": "b/1"}"#;
    assert_eq!(
        diff(plain, one),
        vec![(None, Some("a/1".into()), true, "embedder_changed")]
    );
    assert_eq!(
        diff(one, two),
        vec![(
            Some("a/1".into()),
            Some("b/1".into()),
            false,
            "embedder_changed"
        )]
    );
    assert_eq!(
        diff(one, plain),
        vec![(Some("a/1".into()), None, false, "embedder_changed")]
    );
    assert!(diff(one, one).is_empty());
}

#[test]
fn inference_never_invents_an_embedder() {
    let graph = embedded_modern();
    let inferred = graph.read().traversal().infer_schema().unwrap();
    let vector = inferred.vertices["person"].schema.properties["emb"]
        .vector
        .clone();
    assert_eq!(vector.and_then(|v| v.embedder), None);
}

// ---------------------------------------------------------------------------------------------
// Another element kind: the write quantizes, as any schema write does
// ---------------------------------------------------------------------------------------------

/// `docs(n)` with `emb` declared as vectors of `element` (no dimension, no embedder).
fn docs_declared(n: usize, element: &str, dimension: Option<u32>) -> Arc<Graph> {
    let graph = docs(n);
    {
        let mut lock = graph.write();
        let mut schema = lock.get_schema().unwrap();
        let dimension = dimension
            .map(|d| format!(r#", "dimension": {d}"#))
            .unwrap_or_default();
        let node = schema_with(&format!(
            r#"{{"type": "array", "format": "vector", "element": "{element}"{dimension}}}"#
        ))
        .unwrap()
        .vertices["doc"]
            .schema
            .properties["emb"]
            .clone();
        schema.vertices["doc"]
            .schema
            .properties
            .insert("emb".to_string(), node);
        lock.traversal_mut().set_schema(schema).unwrap();
    }
    graph
}

fn stored_elements(graph: &Graph) -> Vec<String> {
    let lock = graph.read();
    lock.traversal()
        .v(None)
        .values("emb")
        .to_list()
        .unwrap()
        .iter()
        .map(|v| v.as_vector().unwrap().element().name().to_string())
        .collect()
}

#[test]
fn an_f32_embedder_fills_an_i8_or_bit_property_quantized() {
    for element in ["i8", "bit"] {
        let graph = docs_declared(3, element, None);
        let mock = Mock::new("mock", 2);
        let report = run_job(
            &graph,
            &EmbedJob::new("doc", "text", "emb"),
            &registry_with(&mock),
        )
        .unwrap();
        assert_eq!(
            (report.embedded, report.schema_changed),
            (3, true),
            "{element}"
        );
        assert_eq!(stored_elements(&graph), vec![element; 3]);
        // The declaration keeps its element and gets the embedder's model.
        let schema = graph.read().get_schema().unwrap();
        let vector = schema.vertices["doc"].schema.properties["emb"]
            .vector
            .clone()
            .unwrap();
        assert_eq!(vector.element.name(), element);
        assert_eq!(vector.embedder.as_deref(), Some("mock@1"));
        // The same value as a plain write of the f32 vector (one quantizer).
        let lock = graph.read();
        let stored = lock
            .traversal()
            .v(None)
            .has("text", "text 0")
            .values("emb")
            .next()
            .unwrap()
            .unwrap();
        let expected = Mock::vector_of("text 0")
            .quantize(VectorElement::from_name(element).unwrap())
            .unwrap();
        assert_eq!(stored.as_vector(), Some(&expected));
    }
    // Another dimension is still refused before the embedder runs.
    let graph = docs_declared(2, "i8", Some(8));
    let mock = Mock::new("mock", 2);
    let err = run_job(
        &graph,
        &EmbedJob::new("doc", "text", "emb"),
        &registry_with(&mock),
    )
    .unwrap_err();
    assert!(err.contains("expected vector<i8, 8>"), "{err}");
    assert_eq!(mock.calls(), 0);
}

#[test]
fn a_text_query_works_on_i8_and_bit_properties() {
    let embedders = Arc::new(Embedders::default());
    for (element, query) in [
        // i8: the f32 query compares with the stored i8 vectors, as a vector query does.
        ("i8", r#"let q = embed("text 4");"#),
        // bit: the query is quantized like a write, as a bit vector compares only with bits.
        (
            "bit",
            r#"let q = g.inject(embed("text 4")).quantize("bit").next();"#,
        ),
    ] {
        let graph = docs_declared(6, element, Some(256));
        let _ = script(
            &graph,
            &embedders,
            r#"g.embed_property(#{label: "doc", from: "text", to: "emb"})"#,
        )
        .unwrap();
        assert_eq!(stored_elements(&graph), vec![element; 6]);
        for (text, vector) in [
            (
                r#"g.v().nearest("emb", "text 4", 3).values("text").to_list()"#,
                r#"g.v().nearest("emb", q, 3).values("text").to_list()"#,
            ),
            (
                r#"g.v().similarity("emb", "text 4").to_list()"#,
                r#"g.v().similarity("emb", q).to_list()"#,
            ),
        ] {
            let a = script(&graph, &embedders, text).unwrap().to_string();
            let b = script(&graph, &embedders, &format!("{query} {vector}"))
                .unwrap()
                .to_string();
            assert_eq!(a, b, "{element}: {text}");
        }
        let best = script(
            &graph,
            &embedders,
            r#"g.v().nearest("emb", "text 4", 1).values("text").next()"#,
        )
        .unwrap()
        .into_string()
        .unwrap();
        assert_eq!(best, "text 4", "{element}");
    }
}
