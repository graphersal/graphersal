//! Follow-ups from upgrading a real 0.4.0 file (stage U2): the vectors-without-rule notes come
//! ONE per model with one combined schema patch, the saved queries that write a vector property
//! directly are listed (report, notes, fill preview), a schema change of a few labels checks only
//! their elements and its fill estimate includes that check, `g.memory_usage()` reports
//! `model_bytes`, and a closure using `g` inside a chain on `g` gets a Graphersal help.

#![cfg(all(feature = "persist", feature = "script", not(target_arch = "wasm32")))]

use std::sync::Arc;

use graphersal::persist::{ReadOptions, read_snapshot_file};
use graphersal::prelude::*;

use crate::model_fixture::Temp;

fn run(graph: &Arc<Graph>, script: &str) -> Result<String, String> {
    let mut engine = graphersal::script::engine();
    graphersal::script::set_engine_graph(&mut engine, graph.clone());
    let mut scope = graphersal::script::graph_scope(graph.clone());
    engine
        .eval_with_scope::<graphersal::script::rhai::Dynamic>(&mut scope, script)
        .map(|value| value.to_string())
        .map_err(|err| graphersal::script::map_eval_error(err, &Default::default()).to_string())
}

fn ok(graph: &Arc<Graph>, script: &str) -> String {
    run(graph, script).unwrap_or_else(|err| panic!("{script}: {err}"))
}

/// Three labels declare `emb` with `hash-ngram/1` and no source property (`a` holds two
/// vectors, `b` one, `c` none yet; `o` is an unrelated label), and the saved query `loader`
/// writes `a.emb` directly, as the 0.4.0 loader queries did.
fn upgraded_shape() -> Arc<Graph> {
    let graph = Arc::new(Graph::new(TraversalGraph::new()));
    ok(
        &graph,
        r#"
        let emb = #{type: "array", format: "vector", embedder: "hash-ngram/1"};
        let doc = #{schema: #{type: "object", properties: #{text: #{type: "string"}, emb: emb}}};
        g.set_schema(#{mode: "open", vertices: #{a: doc, b: doc, c: doc,
            o: #{schema: #{type: "object", properties: #{name: #{type: "string"}}}}}});
        g.add_v("a").property("text", "graph index").property("emb", embed("graph index")).iterate();
        g.add_v("a").property("text", "vector rule").property("emb", embed("vector rule")).iterate();
        g.add_v("b").property("text", "lock time").property("emb", embed("lock time")).iterate();
        g.add_v("c").property("text", "no vector yet").iterate();
        for i in 0..20 { g.add_v("o").property("name", "o" + i).iterate(); }
        g.define_query(#{name: "loader", body: "fn loader(t) { g.add_v(\"a\").property(\"text\", t).property(\"emb\", embed(t)).iterate() }", params: #{t: "string"}});
        "#,
    );
    graph
}

#[test]
fn a_gsnap_gives_one_vector_note_per_model_and_lists_direct_writers() {
    let temp = Temp::new("u2-notes");
    let graph = upgraded_shape();
    let path = temp.join("shape.gsnap");
    ok(
        &graph,
        &format!("g.export_snapshot({:?})", path.to_string_lossy()),
    );
    let (loaded, report) = read_snapshot_file(&path, &ReadOptions::default()).unwrap();
    // The structured report keeps one entry per property.
    assert_eq!(report.vectors_without_rule.len(), 3);
    let notes = report.notes("shape.gsnap");
    let vector_notes: Vec<&String> = notes
        .iter()
        .filter(|n| n.contains("vectors of hash-ngram/1"))
        .collect();
    assert_eq!(vector_notes.len(), 1, "{notes:#?}");
    let note = vector_notes[0];
    for needle in [
        "3 vector properties are declared with vectors of hash-ngram/1 but have no source \
         property\n",
        "\n  a.emb: 2 vector(s)\n  b.emb: 1 vector(s)\n  c.emb: no element holds one yet\n  \
         vectors: 3 in all\n",
        "graphersal store create <dir> --from shape.gsnap",
        "g.patch_schema(#{\n    vertices: #{\n      a: #{schema: #{properties: #{emb: #{from: \
         \"<source property>\"}}}},\n      b: #{schema: #{properties: #{emb: #{from: \"<source \
         property>\"}}}},\n      c: #{schema: #{properties: #{emb: #{from: \"<source \
         property>\"}}}}\n    }\n  })",
    ] {
        assert!(note.contains(needle), "{needle}\n{note}");
    }
    // The loader writing `emb` directly is listed, never rewritten.
    assert_eq!(report.saved_queries_writing_vectors.len(), 1);
    let write = &report.saved_queries_writing_vectors[0];
    assert_eq!(
        (
            write.query.as_str(),
            write.property.as_str(),
            &write.labels[..]
        ),
        ("loader", "emb", &["a".to_string()][..])
    );
    assert!(!write.rule_exists);
    assert!(
        notes.iter().any(|n| n.starts_with(
            "saved query \"loader\" writes emb directly\n  where: a.emb\n  effect: once the \
             vector rule exists"
        )),
        "{notes:#?}"
    );
    // The combined patch, its placeholders filled in, adds all three rules in one change.
    let combined =
        graphersal::persist::VectorWithoutRule::combined_schema_patch(&report.vectors_without_rule)
            .replace("<source property>", "text");
    let loaded = Arc::new(loaded);
    ok(&loaded, &combined);
    assert_eq!(ok(&loaded, r#"g.v().has("emb").count().next()"#), "4");
    // From now on the loader's direct write is refused.
    let refused = run(&loaded, r#"g.query("loader", #{t: "x"})"#).unwrap_err();
    assert!(refused.contains("emb"), "{refused}");
}

#[test]
fn a_single_property_of_a_model_keeps_its_own_note() {
    let temp = Temp::new("u2-single");
    let graph = Arc::new(Graph::new(TraversalGraph::new()));
    ok(
        &graph,
        r#"g.set_schema(#{mode: "open", vertices: #{doc: #{schema: #{type: "object", properties: #{emb: #{type: "array", format: "vector", embedder: "hash-ngram/1"}}}}}})"#,
    );
    let path = temp.join("one.gsnap");
    ok(
        &graph,
        &format!("g.export_snapshot({:?})", path.to_string_lossy()),
    );
    let (_, report) = read_snapshot_file(&path, &ReadOptions::default()).unwrap();
    let notes = report.notes("one.gsnap");
    assert_eq!(notes.len(), 1, "{notes:#?}");
    assert!(
        notes[0].starts_with(
            "doc.emb is declared with vectors of hash-ngram/1 but has no source property\n  \
             vectors: no element holds one yet"
        ),
        "{}",
        notes[0]
    );
}

#[test]
fn the_fill_preview_names_direct_writers_and_counts_the_schema_check() {
    let graph = upgraded_shape();
    let patch = r#"#{vertices: #{a: #{schema: #{properties: #{emb: #{from: "text"}}}}}}"#;
    let fill = ok(
        &graph,
        &format!(
            "let f = g.validate_schema_patch({patch}).fill[0]; [f.direct_writes, f.scan_elements, f.scan_ms >= 0, f.estimate_ms >= f.scan_ms]"
        ),
    );
    // Only label `a` (2 elements) is checked: the mode stays and no other declaration changed.
    assert_eq!(fill, r#"[["loader"], 2, true, true]"#);
    let warning = ok(
        &graph,
        &format!("g.validate_schema_patch({patch}).fill_warning"),
    );
    assert!(
        warning.contains(
            "Saved queries writing a.emb directly fail once the rule exists (remove that write; \
             they are not rewritten): loader."
        ),
        "{warning}"
    );
    // The approximate preview of `diff_schema` says the same.
    let diff = ok(
        &graph,
        "let s = g.get_schema(); s.vertices.a.schema.properties.emb.from = \"text\"; let f = g.diff_schema(s).fill[0]; [f.direct_writes, f.scan_elements]",
    );
    assert_eq!(diff, r#"[["loader"], 2]"#);
}

/// A schema change of one label in the same mode checks only that label's elements, and still
/// finds their violations; a mode change checks everything.
#[test]
fn a_schema_change_checks_only_the_changed_labels() {
    let graph = upgraded_shape();
    // `o` holds 20 elements, `a` 2: tightening `a` checks 2 (and reports nothing).
    let checked = ok(
        &graph,
        r#"g.validate_schema_patch(#{vertices: #{a: #{schema: #{required: ["text"]}}}}).violations.elements_checked"#,
    );
    assert_eq!(checked, "2");
    // A violation on the changed label is found and refuses the change.
    let refused = run(
        &graph,
        r#"g.patch_schema(#{vertices: #{c: #{schema: #{required: ["emb"]}}}})"#,
    )
    .unwrap_err();
    assert!(refused.contains("emb"), "{refused}");
    // A vertex carrying two changed labels is checked once.
    ok(
        &graph,
        r#"g.add_v("a").add_label("b").property("text", "both").iterate()"#,
    );
    let checked = ok(
        &graph,
        r#"g.validate_schema_patch(#{vertices: #{a: #{schema: #{required: ["text"]}}, b: #{schema: #{required: ["text"]}}}}).violations.elements_checked"#,
    );
    assert_eq!(checked, "4");
    // A mode change checks every element.
    let checked = ok(
        &graph,
        r#"g.validate_schema_patch(#{mode: "closed"}).violations.elements_checked"#,
    );
    assert_eq!(
        checked,
        ok(&graph, "g.v().count().next() + g.e().count().next()")
    );
}

#[test]
fn memory_usage_reports_model_bytes() {
    let graph = upgraded_shape();
    assert_eq!(ok(&graph, "g.memory_usage().model_bytes"), "0");
}

/// A model of the store, once loaded, is counted in `model_bytes` (not in `total_bytes`).
#[cfg(feature = "model2vec")]
#[test]
fn memory_usage_counts_a_loaded_model() {
    let temp = Temp::new("u2-model-bytes");
    let store = graphersal::persist::Store::create(temp.join("s"), crate::model_fixture::options())
        .unwrap();
    crate::model_fixture::import(&store, 1, "acme/tiny");
    let graph = store.graph().clone();
    assert_eq!(ok(&graph, "g.memory_usage().model_bytes"), "0");
    ok(&graph, r#"embed("graph index", #{embedder: "acme/tiny"})"#);
    let bytes: u64 = ok(&graph, "g.memory_usage().model_bytes").parse().unwrap();
    assert!(bytes > 0);
    store.close().unwrap();
}

/// A closure using `g` inside a chain on `g` cannot run (Rhai locks the shared `g` for the
/// whole chain): the error says so and names the forms that work.
#[test]
fn a_closure_over_g_inside_a_chain_on_g_gets_a_help() {
    let graph = upgraded_shape();
    let err = run(&graph, "g.queries().map(|q| g.get_query(q.name).name)").unwrap_err();
    assert!(err.contains("Data race detected"), "{err}");
    assert!(
        err.contains("Help: end the chain on g before the closure runs"),
        "{err}"
    );
    assert!(err.contains("let qs = g.queries();"), "{err}");
    assert_eq!(
        ok(
            &graph,
            "let qs = g.queries(); qs.map(|q| g.get_query(q.name).name)"
        ),
        r#"["loader"]"#
    );
    assert_eq!(
        ok(
            &graph,
            "let names = []; for q in g.queries() { names.push(g.get_query(q.name).name) } names"
        ),
        r#"["loader"]"#
    );
}
