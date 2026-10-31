//! Vector provenance and text queries (book page "Vectors"): the schema keyword `embedder`
//! (parse, print, placement, diff, inference), the text queries of `nearest`/`similarity` on
//! properties filled by direct writes or by a vector rule, their refusals, and the removed job
//! `embed_property` (its call names the vector rule).

#![cfg(feature = "script")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use graphersal::prelude::*;
use graphersal::schema::SchemaChangeKind;
use graphersal::script::rhai::{Dynamic, EvalAltResult};
use graphersal::vector::{
    EmbedContext, EmbedError, Embedder, EmbedderInfo, Embedders, Vector, VectorElement,
};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A host embedder that counts its calls (the only test that needs to count calls; every
/// other test uses the built-in `hash-ngram/1`).
struct Counting {
    info: EmbedderInfo,
    calls: AtomicUsize,
    seen: Mutex<Vec<Vec<String>>>,
}

impl Counting {
    fn new() -> Arc<Counting> {
        Arc::new(Counting {
            info: EmbedderInfo::new("mock", 3, VectorElement::F32)
                .with_model("mock@1")
                .with_max_batch(2),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Embedder for Counting {
    fn info(&self) -> &EmbedderInfo {
        &self.info
    }

    fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .unwrap()
            .push(texts.iter().map(|t| t.to_string()).collect());
        ctx.check()?;
        Ok(texts
            .iter()
            .map(|t| {
                let sum: u32 = t.chars().map(|c| c as u32).sum();
                Vector::from_f32(vec![t.chars().count() as f32, (sum % 1000) as f32, 1.0]).unwrap()
            })
            .collect())
    }
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
    engine
        .eval_with_scope::<Dynamic>(&mut scope, script)
        .map_err(|err| graphersal::script::map_eval_error(err, &Default::default()))
}

fn script_err(graph: &Arc<Graph>, embedders: &Arc<Embedders>, text: &str) -> String {
    match script(graph, embedders, text) {
        Ok(value) => panic!("{text} should fail, gave {value:?}"),
        Err(err) => err.to_string(),
    }
}

/// The modern graph with `emb` written directly (`property("emb", __.embed("name"))`) on
/// every vertex and declared as vectors of `hash-ngram/1` (no rule: the declaration can be
/// changed freely).
fn embedded_modern() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let _ = script(
        &graph,
        &Arc::new(Embedders::default()),
        r#"g.v().property("emb", __.embed("name")).iterate();
           g.set_schema(g.infer_schema(), SchemaMode.open);
           g.patch_schema(#{vertices: #{
               person: #{schema: #{properties: #{emb: #{embedder: "hash-ngram/1"}}}},
               software: #{schema: #{properties: #{emb: #{embedder: "hash-ngram/1"}}}}}})"#,
    )
    .unwrap();
    graph
}

/// The modern graph with `emb` filled by a vector rule on `person` and `software`.
fn ruled_modern() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let rule = r#"#{schema: #{properties: #{emb: #{type: "array", format: "vector", embedder: "hash-ngram/1", from: "name"}}}}"#;
    let _ = script(
        &graph,
        &Arc::new(Embedders::default()),
        &format!(
            r#"g.set_schema(g.infer_schema(), SchemaMode.open);
               g.patch_schema(#{{vertices: #{{person: {rule}, software: {rule}}}}})"#
        ),
    )
    .unwrap();
    graph
}

// ---------------------------------------------------------------------------------------------
// Text queries
// ---------------------------------------------------------------------------------------------

#[test]
fn a_text_query_equals_the_query_with_its_embedded_vector() {
    for graph in [embedded_modern(), ruled_modern()] {
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
}

#[test]
fn the_query_text_is_embedded_once_per_execution() {
    let graph = docs(6);
    let mock = Counting::new();
    let embedder: Arc<dyn Embedder> = mock.clone();
    let embedders = Arc::new(
        Embedders::builder()
            .with(embedder)
            .unwrap()
            .default_name("mock")
            .unwrap()
            .build(),
    );
    let _ = script(
        &graph,
        &embedders,
        r#"g.v().property("emb", __.embed("text")).iterate();
           g.patch_schema(#{vertices: #{doc: #{schema: #{properties: #{emb: #{type: "array", format: "vector", dimension: 3, embedder: "mock@1"}}}}}})"#,
    )
    .unwrap();
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
    let graph = ruled_modern();
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
    for graph in [embedded_modern(), ruled_modern()] {
        let inferred = graph.read().traversal().infer_schema().unwrap();
        let vector = inferred.vertices["person"].schema.properties["emb"]
            .vector
            .clone();
        assert_eq!(vector.and_then(|v| v.embedder), None);
    }
}

// ---------------------------------------------------------------------------------------------
// Another element kind: the rule quantizes, as any schema write does
// ---------------------------------------------------------------------------------------------

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
        let graph = docs(6);
        let _ = script(
            &graph,
            &embedders,
            &format!(
                r#"g.patch_schema(#{{vertices: #{{doc: #{{schema: #{{properties: #{{emb: #{{type: "array", format: "vector", element: "{element}", dimension: 256, embedder: "hash-ngram/1", from: "text"}}}}}}}}}}}})"#
            ),
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

// ---------------------------------------------------------------------------------------------
// The removed job
// ---------------------------------------------------------------------------------------------

#[test]
fn embed_property_was_removed_and_the_error_names_the_vector_rule() {
    let graph = docs(2);
    let embedders = Arc::new(Embedders::default());
    for call in [
        r#"g.embed_property(#{label: "doc", from: "text", to: "emb"})"#,
        r#"g.embedProperty(#{label: "doc", from: "text", to: "emb"})"#,
    ] {
        let err = script_err(&graph, &embedders, call);
        assert!(err.contains("Function not found"), "{err}");
        assert!(err.contains("embed_property was removed in 0.5.0"), "{err}");
        assert!(
            err.contains(r#"from: \"text\""#) || err.contains(r#"from: "text""#),
            "{err}"
        );
        assert!(err.contains("Vector Rules"), "{err}");
    }
    // Nothing was written.
    assert_eq!(stored_elements(&graph).len(), 0);
}
