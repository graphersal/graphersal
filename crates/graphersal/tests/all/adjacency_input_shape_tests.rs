//! The adjacency steps (`out`/`in`/`both`, `outE`/`inE`/`bothE`, `outV`/`inV`/`bothV`/`otherV`)
//! over every stream shape: the accepted ones expand, every other one fails with a typed cast
//! error naming the shape it got, both in the per-traverser path (`toList()`) and in the fused
//! counting path (`.count()`), in both DSL spellings and in the Rust API. Never a panic, never a
//! silent empty result.

use std::sync::Arc;

use graphersal::prelude::*;

thread_local! {
    // Building the engine registers every step; the matrix below runs hundreds of read-only
    // queries, so each test thread builds it (and the graph) once.
    static ENGINE: rhai::Engine = crate::script_host::engine();
    static MODERN: Arc<Graph> = Arc::new(GraphSource::tinkerpop_modern());
}

/// Runs `script` against the modern graph; `Err` is the rendered error report.
fn run(script: &str) -> Result<rhai::Dynamic, String> {
    ENGINE.with(|engine| {
        let mut scope = MODERN.with(|g| crate::script_host::graph_scope(g.clone()));
        engine
            .eval_with_scope::<rhai::Dynamic>(&mut scope, script)
            .map_err(|err| err.to_string())
    })
}

fn int(script: &str) -> i64 {
    run(script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
        .as_int()
        .unwrap_or_else(|t| panic!("{script}: not an int but {t}"))
}

fn names(script: &str) -> Vec<String> {
    let mut out: Vec<String> = run(script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
        .into_array()
        .unwrap_or_else(|t| panic!("{script}: not a list but {t}"))
        .into_iter()
        .map(|v| v.to_string())
        .collect();
    out.sort();
    out
}

fn assert_cast_error(script: &str, got: &str) {
    match run(script) {
        Ok(value) => panic!("{script}: expected a cast error, got {value:?}"),
        Err(report) => assert!(
            report.contains(&format!("got type '{got}'")),
            "{script}: expected a cast error naming '{got}', got:\n{report}"
        ),
    }
}

/// Producers of every stream shape, with the type name the cast error reports.
const SHAPES: &[(&str, &str)] = &[
    (r#"g.V("1")"#, "vertex"),
    (r#"g.V("1").properties("name")"#, "vertexProperty"),
    (r#"g.E("0")"#, "edge"),
    (r#"g.E("0").properties("weight")"#, "edgeProperty"),
    (r#"g.V("1").path()"#, "path"),
    (r#"g.V("1").properties(jpath("name"))"#, "pathProperty"),
    (r#"g.V("1").valueMap().unfold()"#, "entry"),
    (r#"g.V("1").valueMap()"#, "object"),
    (r#"g.V("1").project("n").by("name")"#, "object"),
    (r#"g.V("1").fold()"#, "array"),
    (r#"g.inject(1)"#, "computed"),
    (r#"g.V("1").constant(5)"#, "computed"),
    (r#"g.V().infer_schema()"#, "schema"),
    (r#"g.E().subgraph("sg").cap("sg")"#, "graph"),
];

/// Vertex-expanding steps in both spellings: they accept a vertex and a vertex property (the
/// property's owner).
const VERTEX_STEPS: &[(&str, &str)] = &[
    ("out()", "out()"),
    ("in()", "in_()"),
    ("both()", "both()"),
    ("outE()", "out_e()"),
    ("inE()", "in_e()"),
    ("bothE()", "both_e()"),
];

/// Edge-reading steps in both spellings: they accept an edge and an edge property.
const EDGE_STEPS: &[(&str, &str)] = &[
    ("outV()", "out_v()"),
    ("inV()", "in_v()"),
    ("bothV()", "both_v()"),
];

#[test]
fn vertex_steps_reject_every_non_vertex_shape_in_both_paths() {
    for (producer, got) in SHAPES {
        if matches!(*got, "vertex" | "vertexProperty") {
            continue;
        }
        for (camel, snake) in VERTEX_STEPS {
            for step in [camel, snake] {
                assert_cast_error(&format!("{producer}.{step}.toList()"), got);
                assert_cast_error(&format!("{producer}.{step}.count().next()"), got);
            }
        }
    }
}

#[test]
fn edge_steps_reject_every_non_edge_shape_in_both_paths() {
    for (producer, got) in SHAPES {
        if matches!(*got, "edge" | "edgeProperty") {
            continue;
        }
        for (camel, snake) in EDGE_STEPS.iter().chain([&("otherV()", "other_v()")]) {
            for step in [camel, snake] {
                assert_cast_error(&format!("{producer}.{step}.toList()"), got);
                assert_cast_error(&format!("{producer}.{step}.count().next()"), got);
            }
        }
    }
}

#[test]
fn a_vertex_property_expands_from_its_owner_vertex() {
    // marko: out 3 (lop, vadas, josh), in 0, both 3
    for (step, expected) in [
        ("out()", 3),
        ("in()", 0),
        ("both()", 3),
        ("outE()", 3),
        ("inE()", 0),
        ("bothE()", 3),
    ] {
        let listed = names(&format!(r#"g.V("1").properties("name").{step}.toList()"#)).len();
        assert_eq!(listed as i64, expected, "{step}");
        let count = int(&format!(
            r#"g.V("1").properties("name").{step}.count().next()"#
        ));
        assert_eq!(count, expected, "{step} (fused count)");
    }
    // vadas has one incoming knows edge
    assert_eq!(
        int(r#"g.v("2").values("name").in_e("knows").count().next()"#),
        1
    );
}

#[test]
fn an_edge_property_reads_its_owner_edge_endpoints() {
    let p = r#"g.V("1").outE("knows").has("weight", 0.5).properties("weight")"#;
    assert_eq!(
        names(&format!("{p}.out_v().values(\"name\").to_list()")),
        ["marko"]
    );
    assert_eq!(
        names(&format!("{p}.in_v().values(\"name\").to_list()")),
        ["vadas"]
    );
    assert_eq!(
        names(&format!("{p}.both_v().values(\"name\").to_list()")),
        ["marko", "vadas"]
    );
    assert_eq!(
        names(&format!("{p}.other_v().values(\"name\").to_list()")),
        ["vadas"]
    );
    assert_eq!(int(&format!("{p}.both_v().count().next()")), 2);
    assert_eq!(int(&format!("{p}.other_v().count().next()")), 1);
}

#[test]
fn other_v_without_an_earlier_vertex_explains_the_fix() {
    for script in [
        r#"g.E("0").otherV().toList()"#,
        r#"g.E("0").properties("weight").other_v().toList()"#,
    ] {
        let report = run(script).expect_err(script);
        assert!(report.contains("found no vertex earlier"), "{report}");
        assert!(report.contains("use both_v() there instead"), "{report}");
    }
}

#[test]
fn label_filters_apply_in_the_counting_path() {
    // vertex 4 (josh): out created -> lop, ripple; in knows <- marko
    assert_eq!(int(r#"g.v("4").out("created").count().next()"#), 2);
    assert_eq!(int(r#"g.v("4").in_("created").count().next()"#), 0);
    assert_eq!(
        int(r#"g.v("4").both("knows", "created").count().next()"#),
        3
    );
    assert_eq!(int(r#"g.v("4").both_e("nope").count().next()"#), 0);
    assert_eq!(int(r#"g.v("4").out_e("created").count().next()"#), 2);
    assert_eq!(int(r#"g.v("4").in_e("knows").count().next()"#), 1);
}

#[test]
fn counting_multiplies_by_bulk() {
    // g.V().V("1") yields six marko traversers; barrier() merges them into one with bulk 6; every counting step
    // must multiply by it.
    for (step, per_marko) in [
        ("out()", 3),
        ("both()", 3),
        ("outE()", 3),
        ("bothE()", 3),
        ("in()", 0),
    ] {
        let expanded = int(&format!(r#"g.V().V("1").barrier().{step}.count().next()"#));
        assert_eq!(expanded, 6 * per_marko, "{step}");
    }
    let expanded = int(r#"g.V().outE().barrier().bothV().count().next()"#);
    assert_eq!(expanded, 12);
    let expanded = int(r#"g.V().outE().barrier().outV().count().next()"#);
    assert_eq!(expanded, 6);
    let expanded = int(r#"g.V().outE().barrier().inV().count().next()"#);
    assert_eq!(expanded, 6);
    let expanded = int(r#"g.V().outE().barrier().otherV().count().next()"#);
    assert_eq!(expanded, 6);
}

#[test]
fn rust_api_reports_the_same_cast_errors() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let report = |res: TraverserResult<Vec<ElementProperty>>| -> String {
        format_diagnostic(&*res.expect_err("a cast error"))
    };
    let g = || lock.traversal();
    assert!(report(g().e("0").out(None::<()>).to_list()).contains("got type 'edge'"));
    assert!(report(g().e("0").in_e(None::<()>).count().to_list()).contains("got type 'edge'"));
    assert!(report(g().v("1").fold().both_e(None::<()>).to_list()).contains("got type 'array'"));
    assert!(report(g().v("1").out_v().to_list()).contains("got type 'vertex'"));
    assert!(report(g().v("1").path().in_v().count().to_list()).contains("got type 'path'"));
    assert!(report(g().v("1").both_v().to_list()).contains("got type 'vertex'"));
    assert!(report(g().v("1").other_v().to_list()).contains("got type 'vertex'"));
}
