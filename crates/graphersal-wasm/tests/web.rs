//! Smoke tests of the WebAssembly build, run by `wasm-bindgen-test-runner` (under Node.js):
//! `cargo test -p graphersal-wasm --target wasm32-unknown-unknown`.
#![cfg(target_arch = "wasm32")]

use graphersal::auth::{AccessPolicy, Authorizer};
use graphersal::prelude::*;
use graphersal::schema::PropertySchema;
use graphersal_wasm::{Playground, RunOptions, Session};
use serde_json::Value;
use wasm_bindgen_test::wasm_bindgen_test;

fn json(text: &str) -> Value {
    serde_json::from_str(text).expect("JSON")
}

fn modern() -> Playground {
    let mut p = Playground::new();
    p.load_sample("modern").expect("modern loads");
    p
}

#[wasm_bindgen_test]
fn creates_a_graph_and_runs_a_query() {
    let mut p = Playground::new();
    let info = json(&p.load_sample("modern").unwrap());
    assert_eq!(info["vertexCount"], 6);
    let r = json(
        &p.execute(r#"g.v().has_label("person").values("name")"#, "{}")
            .unwrap(),
    );
    assert_eq!(r["ok"], true);
    assert_eq!(r["terminal"], "execute");
    assert_eq!(r["execution"]["results"].as_array().unwrap().len(), 4);
    assert!(r["display"]["raw"].as_str().unwrap().contains("marko"));
}

#[wasm_bindgen_test]
fn the_statistics_report_the_wasm_memory() {
    let stats = json(&modern().statistics());
    let memory = &stats["process"]["memory"];
    assert!(
        memory["wasmMemoryBytes"].as_u64().unwrap() >= 65536,
        "{stats}"
    );
    assert_eq!(memory["source"], "wasm");
    assert!(memory.get("rssBytes").is_none(), "{stats}");
}

#[wasm_bindgen_test]
fn the_large_generator_builds() {
    let mut p = Playground::new();
    let info = json(&p.load_sample("large").unwrap());
    assert!(info["vertexCount"].as_u64().unwrap() > 50_000, "{info}");
}

#[wasm_bindgen_test]
fn memory_profile_is_tracked_in_the_browser_build() {
    assert!(Playground::memory_tracked());
    let mut p = modern();
    let r = json(
        &p.execute(
            r#"g.v().out().values("name").fold()"#,
            r#"{"profile": true, "profile_types": ["memory"]}"#,
        )
        .unwrap(),
    );
    let metrics = r["execution"]["profile"]["metrics"].as_array().unwrap();
    let fold = metrics.last().unwrap();
    assert_eq!(fold["memory"]["source"], "tracked", "{fold}");
    assert!(
        fold["memory"]["bytes_materialized"].as_u64().unwrap() > 0,
        "{fold}"
    );
    // Without the Memory type there is no memory section at all.
    let plain = json(&p.execute("g.v().count()", r#"{"profile": true}"#).unwrap());
    assert!(
        plain["execution"]["profile"]["metrics"][0]
            .get("memory")
            .is_none()
    );
}

#[wasm_bindgen_test]
fn a_runtime_error_carries_its_step_id() {
    let mut p = modern();
    let r = json(
        &p.execute(
            r#"g.v().has_label("person").union(__.values("name"), __.values("age").property("x", 1))"#,
            r#"{"profile": true}"#,
        )
        .unwrap(),
    );
    assert_eq!(r["ok"], false);
    let error = &r["execution"]["error"];
    assert!(error["step_id"].as_str().unwrap().contains('.'), "{error}");
    assert!(error["help"].is_string(), "{error}");
    assert!(r["execution"]["profile"].is_object(), "partial profile");
}

#[wasm_bindgen_test]
fn graphml_save_and_load_with_the_schema_first_keeps_schema_and_types() {
    let mut session = Session::new();
    session.load_sample("modern").unwrap();
    {
        let graph = session.graph().clone();
        let mut schema = graph.inferred_schema().with_mode(SchemaMode::Closed);
        schema
            .vertices
            .get_mut("person")
            .unwrap()
            .schema
            .properties
            .insert("uid".to_string(), PropertySchema::uuid().nullable());
        graph.set_schema(schema).unwrap();
    }
    let opts = RunOptions::default();
    let set = json(&session.execute(
        r#"g.v(1).property("uid", UUID.fromString("550e8400-e29b-41d4-a716-446655440000"))"#,
        &opts,
    ));
    assert_eq!(set["ok"], true, "{set}");
    let saved = session.save_graphml().unwrap();
    assert_eq!(saved["warnings"], serde_json::json!([]), "{saved}");

    // The schema is saved separately and loaded with the graph; the GraphML carries none.
    let mut p = Playground::new();
    let schema_json = saved["schema"].as_str().expect("schema JSON").to_string();
    let info = json(
        &p.load_graphml(
            saved["text"].as_str().unwrap(),
            "saved.graphml",
            Some(schema_json),
        )
        .unwrap(),
    );
    assert_eq!(info["vertexCount"], 6);
    assert_eq!(info["edgeCount"], 6);
    assert_eq!(info["schemaMode"], "closed");
    let schema = json(&p.schema());
    assert_eq!(schema["mode"], "closed");
    assert_eq!(schema["source"], "stored");
    let stored = json(&p.get_schema());
    assert_eq!(stored["mode"], "closed");
    assert!(
        json(&p.diff_schema(&p.get_schema()).unwrap())["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let uid = json(
        &p.execute(r#"g.v(1).values("uid").as_string().next()"#, "{}")
            .unwrap(),
    );
    assert_eq!(
        uid["execution"]["results"][0], "550e8400-e29b-41d4-a716-446655440000",
        "{uid}"
    );
    let typed = json(
        &p.execute(
            r#"g.v(1).values("uid").is(P.typeOf(GType.UUID)).count().next()"#,
            "{}",
        )
        .unwrap(),
    );
    assert_eq!(
        typed["execution"]["results"][0], 1,
        "the uuid type survives: {typed}"
    );
    // Loading the same file again without the schema file drops the schema: it is never
    // carried over from the previous graph.
    let info = json(
        &p.load_graphml(saved["text"].as_str().unwrap(), "saved.graphml", None)
            .unwrap(),
    );
    assert_eq!(info["schemaMode"], serde_json::Value::Null, "{info}");
    assert_eq!(p.get_schema(), "null");
    // A bad schema file fails the load and keeps the current graph.
    assert!(
        p.load_graphml(
            saved["text"].as_str().unwrap(),
            "x.graphml",
            Some("{bad".into())
        )
        .is_err()
    );
    assert_eq!(json(&p.info())["name"], "saved.graphml");
}

#[wasm_bindgen_test]
fn graphson_save_and_load_keeps_types_and_reports_skips() {
    let mut p = modern();
    let set = json(
        &p.execute(
            r#"g.v(1).property("uid", UUID.fromString("550e8400-e29b-41d4-a716-446655440000")).property("tags", ["a"]).next()"#,
            "{}",
        )
        .unwrap(),
    );
    assert_eq!(set["ok"], true, "{set}");
    let saved = json(&p.save_graphson().unwrap());
    assert_eq!(saved["warnings"], serde_json::json!([]), "{saved}");
    let mut q = Playground::new();
    let info = json(
        &q.load_graphson(saved["text"].as_str().unwrap(), "saved.json", None)
            .unwrap(),
    );
    assert_eq!(info["vertexCount"], 6);
    assert_eq!(info["edgeCount"], 6);
    let typed = json(
        &q.execute(
            r#"g.v(1).values("uid").is(P.typeOf(GType.UUID)).count().next()"#,
            "{}",
        )
        .unwrap(),
    );
    assert_eq!(typed["execution"]["results"][0], 1, "{typed}");
    // A date/time type Graphersal cannot hold (a year) is skipped and listed in the warnings,
    // one it holds (`g:Date`) is imported; malformed text is an error, not a panic.
    let info = json(
        &q.load_graphson(
            r#"{"id":"v","properties":{"born":[{"value":{"@type":"gx:Year","@value":"2016"}}],"seen":[{"value":{"@type":"g:Date","@value":1}}]}}"#,
            "dates.json",
            None,
        )
        .unwrap(),
    );
    let warnings = info["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{info}");
    assert!(warnings[0].as_str().unwrap().contains("gx:Year"), "{info}");
    let seen = json(
        &q.execute(r#"g.v().has("seen").count().next()"#, "{}")
            .unwrap(),
    );
    assert_eq!(seen["execution"]["results"][0], 1, "{seen}");
    assert!(q.load_graphson("{bad", "bad.json", None).is_err());
    assert!(GraphSource::from_file("graph.json").is_err());
}

#[wasm_bindgen_test]
fn file_io_returns_an_error_instead_of_panicking() {
    // There is no file system in the browser: every path-based call is an error value.
    assert!(GraphSource::from_file("graph.graphml").is_err());
    let mut p = modern();
    let r = json(
        &p.execute(r#"g.import_graphml("graph.graphml")"#, "{}")
            .unwrap(),
    );
    assert_eq!(r["ok"], false, "{r}");
    let r = json(
        &p.execute(r#"g.export_graphml("out.graphml")"#, "{}")
            .unwrap(),
    );
    assert_eq!(r["ok"], false, "{r}");
    // The session still works afterwards.
    let r = json(&p.execute("g.v().count().next()", "{}").unwrap());
    assert_eq!(r["execution"]["results"][0], 6);
}

#[wasm_bindgen_test]
fn file_access_and_import_are_denied_without_panicking() {
    // The playground allows everything but files: a file call and Rhai's `import` are a clear
    // `PermissionDenied`, never a panic.
    let mut p = modern();
    for script in [
        r#"g.export_graphml("out.graphml")"#,
        r#"import "module" as m; 1"#,
        r#"GraphSource::file("graph.graphml")"#,
    ] {
        let r = json(&p.execute(script, "{}").unwrap());
        assert_eq!(r["ok"], false, "{r}");
        assert!(r.to_string().contains("Permission denied"), "{script}: {r}");
    }
    // A file root cannot be resolved on wasm32 (no file system): denied, never a panic.
    let rooted = AccessPolicy::allow_all().with_file_root("/data");
    assert!(
        rooted
            .resolve_file(std::path::Path::new("x.graphml"))
            .is_err()
    );
    // Without a root the path passes through; the I/O itself then fails as before.
    assert!(
        AccessPolicy::allow_all()
            .resolve_file(std::path::Path::new("x.graphml"))
            .is_ok()
    );
    let r = json(&p.execute("g.v().count().next()", "{}").unwrap());
    assert_eq!(r["execution"]["results"][0], 6);
}

/// The drawing is columnar; the inspector fetches one element's properties when it is clicked.
#[wasm_bindgen_test]
fn graph_view_is_columnar_and_element_fetches_the_properties() {
    let p = modern();
    let view = json(&p.graph_view(100, 100));
    assert_eq!(view["vertexIds"].as_array().unwrap().len(), 6);
    assert_eq!(view["edgeTarget"].as_array().unwrap().len(), 6);
    assert!(view.get("nodes").is_none());
    let marko = json(&p.element("vertex", "1").unwrap());
    assert_eq!(marko["properties"]["name"], "marko");
    assert!(json(&p.element("edge", "99").unwrap()).is_null());
}

/// The engine's force layout in the browser build: slices, positions as a Float32Array (a
/// `Vec<f32>` here), the cache, cancellation.
#[wasm_bindgen_test]
fn the_force_layout_runs_in_slices_and_answers_positions() {
    let mut p = modern();
    let view = json(&p.graph_view(100, 100));
    let column = |key: &str| -> Vec<u32> {
        view[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect()
    };
    let (source, target) = (column("edgeSource"), column("edgeTarget"));
    let mut status = json(
        &p.layout_start(6, source.clone(), target.clone(), "", 0.0)
            .unwrap(),
    );
    assert_eq!(status["state"], "running", "{status}");
    let job = status["job"].as_u64().unwrap() as u32;
    while status["state"] == "running" {
        status = json(&p.layout_step(job, 50.0).unwrap());
    }
    assert_eq!(status["state"], "done");
    let positions = p.layout_positions(job).unwrap();
    assert_eq!(positions.len(), 12);
    assert!(positions.iter().all(|v| v.is_finite()));
    // The same drawing again: from the cache, the same positions.
    let again = json(&p.layout_start(6, source, target, "{}", 50.0).unwrap());
    assert_eq!(again["state"], "done");
    assert_eq!(again["cached"], true);
    let job = again["job"].as_u64().unwrap() as u32;
    assert_eq!(p.layout_positions(job).unwrap(), positions);
    // Another seed, cancelled after the first slice: positions reached so far.
    let other = json(
        &p.layout_start(6, vec![0], vec![1], r#"{"seed": 5}"#, 0.0)
            .unwrap(),
    );
    let job = other["job"].as_u64().unwrap() as u32;
    assert_eq!(json(&p.layout_cancel(job).unwrap())["state"], "cancelled");
    assert_eq!(p.layout_positions(job).unwrap().len(), 12);
}

#[wasm_bindgen_test]
fn completions_come_from_the_engine() {
    let list = json(&Playground::new().completions());
    assert!(list.as_array().unwrap().len() > 100);
    let tokens = json(&Playground::new().token_values());
    assert_eq!(
        tokens["Order"],
        serde_json::json!(["asc", "desc", "shuffle"])
    );
    assert!(tokens.get("Execution").is_none(), "{tokens}");
}

/// The store in memory in the browser build: a loaded graph is a store, commits are journaled,
/// the automatic checkpoint runs synchronously (no threads), the Store menu's operations work,
/// a fork is a second database.
#[wasm_bindgen_test]
fn a_loaded_graph_is_a_store_in_memory_in_the_browser() {
    let mut p = modern();
    let info = json(&p.store_info().unwrap());
    assert_eq!(info["memory"], true);
    for i in 0..3 {
        let r = json(
            &p.execute(&format!("g.add_v(\"p\").property(\"i\", {i}).next()"), "{}")
                .unwrap(),
        );
        assert_eq!(r["ok"], true, "{r}");
    }
    assert_eq!(json(&p.store_info().unwrap())["commitSeq"], 3);
    p.mark("three").unwrap();
    json(&p.checkpoint("cp").unwrap());
    let rolled = json(&p.rollback(r#"{"target": {"commit": 1}}"#).unwrap());
    assert_eq!(rolled["graph"]["vertexCount"], 7);
    let fork = json(&p.fork(r#"{"target": {"commit": 0}}"#).unwrap());
    assert_eq!(fork["store"]["databases"].as_array().unwrap().len(), 2);
    assert_eq!(json(&p.verify_store().unwrap())["verify"]["ok"], true);
    assert!(p.open_view("not json").is_err());
}

/// `Fake(seed)` gives the same values in the browser build as natively (the reproducibility
/// contract of `graphersal`'s `script/fake`; `fake_tests.rs` pins the same string natively, so a
/// platform difference in the generator or the float formatting shows here).
/// The built-in embedder runs in the browser: the `embed` step, `embed(text)` and
/// `g.embedders()`.
#[wasm_bindgen_test]
fn embed_runs_in_the_browser() {
    let mut p = modern();
    let r = json(
        &p.execute(
            r#"g.v().has_label("person").property("emb", __.embed("name")).iterate();
               [g.v().has_label("person").nearest("emb", embed("marco"), 1).values("name").next(),
                g.embedders()[0].name]"#,
            "{}",
        )
        .unwrap(),
    );
    assert_eq!(r["ok"], true, "{r}");
    let raw = r["display"]["raw"].as_str().unwrap();
    assert!(raw.contains("marko") && raw.contains("hash-ngram/1"), "{r}");
}

#[wasm_bindgen_test]
fn fake_values_match_the_native_build() {
    let mut p = Playground::new();
    let r = json(
        &p.execute(
            r#"let f = Fake(42); `${f.full_name()}|${f.int(1, 1000000)}|${f.float()}|${f.uuid()}|${f.fork("x").email()}`"#,
            "{}",
        )
        .unwrap(),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(
        r["display"]["raw"].as_str().unwrap().contains(
            "Lawrence Young|278602|0.34419071652363753|09bc585a-2448-43f2-9e44-31fa3c80db06|philip.davis70@example.org"
        ),
        "{r}"
    );
}

/// Saved queries in the browser build: define (and a refused definition with its field), list,
/// the call text runs, the catalog file round-trips into another playground, drop.
#[wasm_bindgen_test]
fn saved_queries_are_managed_and_run_in_the_browser() {
    let mut p = modern();
    let spec = r#"{"name": "people", "folder": "demo", "params": [{"name": "limit", "schema": {"type": "integer", "minimum": 1}, "default": 2}], "body": "fn people(limit) { g.v().has_label(\"person\").values(\"name\").limit(limit) }"}"#;
    let answer = json(&p.define_query(spec).unwrap());
    assert_eq!(answer["ok"], true, "{answer}");
    let refused = json(
        &p.define_query(r#"{"name": "1x", "body": "fn x() {}"}"#)
            .unwrap(),
    );
    assert_eq!(refused["errors"][0]["field"], "name");
    let list = json(&p.list_queries(Some("demo".into())));
    assert_eq!(list[0]["status"], "ok");
    let text = json(&p.query_call_text("people", r#"{"limit": 3}"#).unwrap());
    assert_eq!(text, "g.query(\"people\", #{limit: 3})");
    let r = json(&p.execute(text.as_str().unwrap(), "{}").unwrap());
    assert_eq!(
        r["execution"]["results"].as_array().unwrap().len(),
        3,
        "{r}"
    );
    let file = p.save_catalog();
    let mut other = modern();
    let loaded = json(&other.load_catalog(&file, false).unwrap());
    assert_eq!(loaded["graph"]["savedQueries"], 1);
    assert_eq!(json(&p.drop_query("people").unwrap())["dropped"], true);
    assert_eq!(json(&p.info())["savedQueries"], 0);
}

/// Compression rules in the browser build: define (a refused rule names its field), list with
/// stats, reads stay plain, recompress, the catalog file carries the rule, drop.
#[wasm_bindgen_test]
fn compression_rules_in_the_browser() {
    let mut p = modern();
    let defined = json(
        &p.define_compression(
            r#"{"name": "names", "label": "person", "path": "name", "minBytes": 1}"#,
        )
        .unwrap(),
    );
    assert_eq!(defined["ok"], true, "{defined}");
    let refused = json(
        &p.define_compression(r#"{"name": "x", "label": "person", "path": "a[0]"}"#)
            .unwrap(),
    );
    assert_eq!(refused["errors"][0]["field"], "path");
    assert_eq!(json(&p.list_compressions())[0]["name"], "names");
    let r = json(
        &p.execute(
            r#"g.v().has_label("person").values("name").to_list()"#,
            "{}",
        )
        .unwrap(),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert!(r.to_string().contains("marko"), "{r}");
    assert_eq!(json(&p.recompress("names").unwrap())["name"], "names");
    let file = p.save_catalog();
    assert!(file.contains("\"compression\""), "{file}");
    assert_eq!(json(&p.drop_compression("names").unwrap())["dropped"], true);
    assert_eq!(json(&p.info())["compressionRules"], 0);
}

/// Runs one GQL query through the browser session: it either answers (with `rows` rows) or is
/// refused by the parser's nesting limits (GQLSTATUS 54001), never a stack overflow of the
/// 1 MB wasm32 stack (debug build, the deepest frames).
fn gql_within_limits(p: &mut Playground, query: &str, rows: Option<usize>) {
    let r = json(&p.execute(&format!("g.gql(\"{query}\")"), "{}").unwrap());
    match rows {
        Some(rows) => {
            assert_eq!(r["ok"], true, "{query}: {r}");
            assert_eq!(
                r["execution"]["results"].as_array().unwrap().len(),
                rows,
                "{query}: {r}"
            );
        }
        None => {
            assert_eq!(r["ok"], false, "{query}: {r}");
            assert!(r.to_string().contains("54001"), "{query}: {r}");
        }
    }
}

#[wasm_bindgen_test]
fn deeply_nested_gql_stays_within_the_wasm_stack() {
    let mut p = modern();
    let nest = |n: usize, inner: &str| format!("{}{inner}{}", "(".repeat(n), ")".repeat(n));
    let ors = |n: usize| {
        (0..n)
            .map(|i| format!("p.age = {i}"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let q = |predicate: &str| format!("MATCH (p:person) WHERE {predicate} RETURN p.name AS n");
    // Parentheses: the deepest accepted nesting, and one past it.
    gql_within_limits(&mut p, &q(&nest(120, "p.age > 1")), Some(4));
    gql_within_limits(&mut p, &q(&nest(126, "p.age > 1")), Some(4));
    gql_within_limits(&mut p, &q(&nest(127, "p.age > 1")), None);
    // Flat OR chains: the tallest accepted operator tree, and one past it.
    gql_within_limits(&mut p, &q(&ors(500)), Some(4));
    gql_within_limits(&mut p, &q(&ors(510)), Some(4));
    gql_within_limits(&mut p, &q(&ors(511)), None);
    // Both at once: a tall tree under deep parentheses.
    gql_within_limits(&mut p, &q(&nest(120, &ors(500))), Some(4));
    // Other recursive forms at their deepest.
    let nots = format!("{}p.age > 1", "NOT NOT ".repeat(60));
    gql_within_limits(&mut p, &q(&nots), Some(4));
    let minus = format!("p.age > {}1", "- ".repeat(126));
    gql_within_limits(&mut p, &q(&minus), Some(4));
    let lists = format!("size({}1{}) = 1", "[".repeat(125), "]".repeat(125));
    gql_within_limits(&mut p, &q(&lists), Some(4));
    let calls = format!(
        "{}p.name{} IS NOT NULL",
        "upper(".repeat(125),
        ")".repeat(125)
    );
    gql_within_limits(&mut p, &q(&calls), Some(4));
    let cases = format!(
        "{}true{} = true",
        "CASE WHEN true THEN ".repeat(120),
        " END".repeat(120)
    );
    gql_within_limits(&mut p, &q(&cases), Some(4));
    let labels = format!("MATCH (p:{}) RETURN p.name AS n", nest(126, "person"));
    gql_within_limits(&mut p, &labels, Some(4));
}

/// Stage 2d: the deepest accepted nesting of `CALL { }` and whole-query subquery bodies
/// (`bind::MAX_BODY_DEPTH`, 16), each with the tallest expression inside, stays within the 1 MB
/// wasm32 stack; one level more is refused (54001). Before their lowering landed (2d.T5 for
/// `CALL`, 2d.T5c for whole-query bodies) the accepted ones answered "not yet supported"
/// (0A000) after binding; both forms now run.
#[wasm_bindgen_test]
fn deeply_nested_gql_bodies_stay_within_the_wasm_stack() {
    let mut p = modern();
    let ors = |n: usize| {
        (0..n)
            .map(|i| format!("x = {i}"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let calls = |levels: usize| {
        let mut q = format!("WITH 1 AS x RETURN {} AS y", ors(500));
        for _ in 0..levels {
            q = format!("CALL {{ {q} UNION ALL RETURN 2 AS y }} RETURN y");
        }
        q
    };
    let bodies = |levels: usize| {
        let mut q = format!("EXISTS {{ LET x = 1 FILTER {} RETURN x }}", ors(440));
        for _ in 1..levels {
            q = format!("EXISTS {{ MATCH (n) WITH n WHERE {q} RETURN n }}");
        }
        format!("MATCH (n) WHERE {q} RETURN n")
    };
    for (query, refused) in [
        (calls(16), false),
        (calls(17), true),
        (bodies(16), false),
        (bodies(17), true),
    ] {
        let r = json(&p.execute(&format!("g.gql(\"{query}\")"), "{}").unwrap());
        let text = r.to_string();
        if refused {
            assert!(text.contains("54001"), "{}: {text}", &query[..60]);
        } else {
            assert!(
                r["ok"] == true || text.contains("0A000"),
                "{}: {text}",
                &query[..60]
            );
        }
    }
}

/// Stage 2d.T5: the deepest accepted `CALL { }` nesting (`bind::MAX_BODY_DEPTH`, 16) lowers and
/// runs within the 1 MB wasm32 stack, each level a `UNION ALL` body or a correlated body
/// importing the row, the tallest expression innermost.
#[wasm_bindgen_test]
fn deeply_nested_calls_run_within_the_wasm_stack() {
    let mut p = modern();
    let ors = (0..500)
        .map(|i| format!("x = {i}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut unions = format!("WITH 1 AS x RETURN {ors} AS y");
    let mut imports = format!("MATCH (p:person) WITH p, 1 AS x RETURN {ors} AS y");
    for _ in 0..16 {
        unions = format!("CALL {{ {unions} UNION ALL RETURN 2 AS y }} RETURN y");
        imports = format!("MATCH (p:person) CALL (p) {{ {imports} }} RETURN y");
    }
    for query in [unions, imports] {
        let r = json(&p.execute(&format!("g.gql(\"{query}\")"), "{}").unwrap());
        assert!(r["ok"] == true, "{}: {r}", &query[..60]);
    }
}

/// Stage 2d.T5c: the deepest accepted whole-query subquery nesting (`bind::MAX_BODY_DEPTH`, 16
/// levels of `EXISTS` bodies, or of `COLLECT` bodies around a `COUNT`), the tallest expression
/// innermost, lowers and runs within the 1 MB wasm32 stack.
#[wasm_bindgen_test]
fn deeply_nested_whole_query_bodies_run_within_the_wasm_stack() {
    let mut p = modern();
    let ors = (0..440)
        .map(|i| format!("x = {i}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut exists = format!("EXISTS {{ LET x = 1 FILTER {ors} RETURN x }}");
    let mut collects = format!("COUNT {{ LET x = 1 FILTER {ors} RETURN x }}");
    for _ in 1..16 {
        exists = format!("EXISTS {{ MATCH (n) WITH n WHERE {exists} RETURN n }}");
        collects =
            format!("size(COLLECT {{ MATCH (n) WITH n WHERE {collects} > 0 RETURN n.name }})");
    }
    for query in [
        format!("MATCH (n) WHERE {exists} RETURN n"),
        format!("MATCH (n) RETURN {collects} AS c"),
    ] {
        let r = json(&p.execute(&format!("g.gql(\"{query}\")"), "{}").unwrap());
        assert!(r["ok"] == true, "{}: {r}", &query[..60]);
    }
}

/// Stage 2d.T4: `UNION` operands and `NEXT` blocks lower and run the tallest expression (a
/// substituted `LET` and a column read from the rows before) within the 1 MB wasm32 stack.
#[wasm_bindgen_test]
fn tall_expressions_in_union_operands_stay_within_the_wasm_stack() {
    let mut p = modern();
    let ors = |n: usize| {
        (0..n)
            .map(|i| format!("x = {i}"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    for query in [
        format!("RETURN 0 AS y UNION ALL LET x = 1 RETURN {} AS y", ors(500)),
        format!(
            "RETURN 0 AS y UNION ALL WITH 1 AS x RETURN {} AS y",
            ors(500)
        ),
        format!(
            "RETURN 0 AS x UNION ALL RETURN 1 AS x NEXT RETURN {} AS y UNION RETURN x = 0 AS y",
            ors(500)
        ),
    ] {
        let r = json(&p.execute(&format!("g.gql(\"{query}\")"), "{}").unwrap());
        assert!(r["ok"] == true, "{}: {r}", &query[..60]);
    }
}

/// Stage 2d.T6: the list functions `reduce`, `any` and `all` at the deepest nesting the parser
/// accepts (in `reduce`'s `|` part, in its initial value, in the list of `any`, inside a list
/// comprehension's `WHERE`) run within the 1 MB wasm32 stack; one level more is 54001.
#[wasm_bindgen_test]
fn deeply_nested_list_functions_run_within_the_wasm_stack() {
    let mut p = modern();
    let nest = |form: &str, depth: usize| {
        let mut expr = "1".to_string();
        for i in 0..depth {
            expr = match form {
                "reduce" => format!("reduce(s{i} = 0, x{i} IN [1] | s{i} + {expr})"),
                "init" => format!("reduce(s{i} = {expr}, x{i} IN [1] | s{i})"),
                "any" => format!("any(x{i} IN [{expr}] WHERE true)"),
                _ => {
                    format!("head([x{i} IN [1] WHERE all(y{i} IN [x{i}] WHERE y{i} > 0) | {expr}])")
                }
            };
        }
        format!("RETURN {expr} AS c")
    };
    for (form, deepest) in [("reduce", 42), ("init", 63), ("any", 42), ("all", 41)] {
        gql_within_limits(&mut p, &nest(form, deepest), Some(1));
        gql_within_limits(&mut p, &nest(form, deepest + 1), None);
    }
}
