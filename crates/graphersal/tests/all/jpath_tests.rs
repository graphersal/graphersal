#![cfg(all(feature = "script", feature = "serde"))]
//! Rhai surface of the `JsonPath` property-key type: the `jpath("...")` constructor.

use crate::script_host::{eval_value, rhai};
use graphersal::prelude::*;
use graphersal::schema::{Constraints, EdgeSchema, PropertySchema, SchemaPatch};
use std::sync::Arc;

fn run(script: &str) -> Result<rhai::Dynamic, String> {
    eval_value(Arc::new(GraphSource::tinkerpop_modern()), script).map_err(|e| e.to_string())
}

fn text(script: &str) -> String {
    run(script)
        .expect("script failed")
        .into_string()
        .expect("string result")
}

#[test]
fn valid_paths_render_canonically() {
    assert_eq!(text(r#"jpath("a.b[1].c").to_string()"#), "$.a.b[1].c");
    assert_eq!(text(r#"jpath("$.a.b[1].c").to_string()"#), "$.a.b[1].c");
    assert_eq!(text(r#"jpath("name").to_string()"#), "$.name");
}

#[test]
fn bracket_and_negative_forms() {
    assert_eq!(text(r#"jpath("a[-1]").to_string()"#), "$.a[-1]");
    assert_eq!(text(r#"jpath("['a.b'].c").to_string()"#), "$['a.b'].c");
    assert_eq!(text(r#"jpath("[\"x y\"][0]").to_string()"#), "$['x y'][0]");
}

#[test]
fn invalid_path_is_a_script_error() {
    for bad in ["", "$", "a..b", "a[", "a[*]", "a.b."] {
        let err = run(&format!("jpath(\"{bad}\")")).expect_err("must fail");
        assert!(
            err.to_lowercase().contains("json"),
            "unexpected error for {bad:?}: {err}"
        );
    }
    let err = run(r#"jpath("a..b")"#).expect_err("must fail");
    assert!(err.contains("a..b"), "{err}");
}

#[test]
fn equality() {
    assert!(
        run(r#"jpath("a.b") == jpath("$.a.b")"#)
            .unwrap()
            .as_bool()
            .unwrap()
    );
    assert!(
        run(r#"jpath("a.b") != jpath("a.c")"#)
            .unwrap()
            .as_bool()
            .unwrap()
    );
    assert!(
        !run(r#"jpath("a.b") == jpath("a[0]")"#)
            .unwrap()
            .as_bool()
            .unwrap()
    );
}

#[test]
fn works_inside_arrays_and_maps() {
    assert_eq!(
        text(r#"let xs = [jpath("a.b"), jpath("c[0]")]; xs[1].to_string()"#),
        "$.c[0]"
    );
    assert_eq!(
        run(r#"let xs = [jpath("a"), jpath("b")]; xs.len()"#)
            .unwrap()
            .as_int()
            .unwrap(),
        2
    );
    assert_eq!(
        text(r#"let m = #{p: jpath("a.b")}; m.p.to_string()"#),
        "$.a.b"
    );
    assert!(
        run(r#"let xs = [jpath("a")]; xs[0] == jpath("$.a")"#)
            .unwrap()
            .as_bool()
            .unwrap()
    );
    assert_eq!(text(r#"type_of(jpath("a"))"#), "JsonPath");
}

// ---------------------------------------------------------------------------------------------
// Task 5: `values` / `properties` / `has*` accept a `PropKey` (a plain string or a `JsonPath`).
// ---------------------------------------------------------------------------------------------

use graphersal::prelude::JsonPath;

fn jp(text: &str) -> JsonPath {
    text.parse().expect("valid path")
}

/// The modern graph plus nested properties:
/// v1 (marko) meta {tags: [x,y,z], k: 1, nested: {deep: {n: 5}}, items: [{n:1},{n:2}]},
/// v2 (vadas) meta {tags: [q], k: 2}, v4 (josh) meta "scalar" (a type mismatch for any path
/// below it), v3 (lop) has no meta, v6 (peter) has a literal property named `a.b` and a nested
/// `a: {b: 7}`. Edge "0" (marko -> vadas, knows) carries info {rank: 3, list: [10, 20]}.
fn nested() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    let setup = r#"
        g.V("1").property_json(#{meta: #{tags: ["x", "y", "z"], k: 1, nested: #{deep: #{n: 5}}, items: [#{n: 1}, #{n: 2}]}}).toList();
        g.V("2").property_json(#{meta: #{tags: ["q"], k: 2}}).toList();
        g.V("4").property("meta", "scalar").toList();
        g.V("6").property("a.b", 9).toList();
        g.V("6").property_json(#{a: #{b: 7}}).toList();
        g.E("0").property("info", #{rank: 3, list: [10, 20]}).toList();
        1
    "#;
    let _ = run_on(&graph, setup).expect("fixture");
    graph
}

fn run_on(graph: &Arc<Graph>, script: &str) -> Result<rhai::Dynamic, String> {
    eval_value(graph.clone(), script).map_err(|e| e.to_string())
}

/// The elements of a `toList()` result as sorted display strings.
fn list(graph: &Arc<Graph>, script: &str) -> Vec<String> {
    let value = run_on(graph, script).unwrap_or_else(|e| panic!("{script}: {e}"));
    let array = value
        .into_array()
        .unwrap_or_else(|_| panic!("{script}: not an array"));
    let mut out: Vec<String> = array.iter().map(|v| v.to_string()).collect();
    out.sort();
    out
}

fn rust_values(
    graph: &Arc<Graph>,
    build: impl for<'a> FnOnce(
        GraphTraversalSource<'a, &'a TraversalGraph>,
    ) -> GraphTraversalSource<'a, &'a TraversalGraph>,
) -> Vec<String> {
    let lock = graph.read();
    let mut out: Vec<String> = build(lock.traversal())
        .to_list()
        .expect("traversal")
        .iter()
        .map(|v| match v {
            ElementProperty::String(s) => s.to_string(),
            other => other
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| format!("{other:?}")),
        })
        .collect();
    out.sort();
    out
}

#[test]
fn values_reads_nested_paths_in_both_apis_and_spellings() {
    let g = nested();
    for script in [
        r#"g.V().values(jpath("meta.tags[0]")).toList()"#,
        r#"g.v().values(jpath("$.meta.tags[0]")).toList()"#,
        r#"g.V().values([jpath("meta.tags[0]")]).toList()"#,
    ] {
        assert_eq!(list(&g, script), ["q", "x"], "{script}");
    }
    assert_eq!(
        rust_values(&g, |t| t.v(None::<()>).values(jp("meta.tags[0]"))),
        ["q", "x"]
    );
    assert_eq!(
        rust_values(&g, |t| t.v(None::<()>).values(jp("meta.nested.deep.n"))),
        ["5"]
    );
    assert_eq!(
        rust_values(&g, |t| t.v(None::<()>).values(jp("meta.items[1].n"))),
        ["2"]
    );
    // A path to a subtree yields the whole subtree.
    assert_eq!(
        list(&g, r#"g.V("2").values(jpath("meta.tags")).toList()"#).len(),
        1
    );
}

#[test]
fn negative_index_reads_from_the_end() {
    let g = nested();
    assert_eq!(
        list(&g, r#"g.V().values(jpath("meta.tags[-1]")).toList()"#),
        ["q", "z"]
    );
    assert_eq!(
        rust_values(&g, |t| t.v(None::<()>).values(jp("meta.tags[-3]"))),
        ["x"]
    );
    assert!(list(&g, r#"g.V().values(jpath("meta.tags[-4]")).toList()"#).is_empty());
}

#[test]
fn missing_paths_and_type_mismatches_are_absent_never_errors() {
    let g = nested();
    for path in [
        "meta.nope",    // missing key
        "meta.tags[9]", // index out of range
        "meta.tags.x",  // key on an array
        "meta.k[0]",    // index on a scalar
        "meta.k.x",     // descent through a scalar
        "meta[0]",      // index on an object
        "nope.x",       // missing top-level property
    ] {
        let script = format!(r#"g.V().values(jpath("{path}")).toList()"#);
        assert!(list(&g, &script).is_empty(), "{path}");
        let script = format!(r#"g.V().has(jpath("{path}")).count().next()"#);
        assert_eq!(run_on(&g, &script).unwrap().as_int().unwrap(), 0, "{path}");
        let script = format!(r#"g.V().hasNot(jpath("{path}")).count().next()"#);
        assert_eq!(run_on(&g, &script).unwrap().as_int().unwrap(), 6, "{path}");
    }
    // v4's `meta` is a string: every path below it is absent, the traversal still succeeds.
    assert!(list(&g, r#"g.V("4").values(jpath("meta.k")).toList()"#).is_empty());
    assert!(rust_values(&g, |t| t.v("4").values(jp("meta.k"))).is_empty());
}

#[test]
fn single_segment_path_equals_the_plain_name() {
    let g = nested();
    assert_eq!(
        list(&g, r#"g.V().values(jpath("name")).toList()"#),
        list(&g, r#"g.V().values("name").toList()"#)
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("name"), "marko").values("age").toList()"#
        ),
        ["29"]
    );
}

#[test]
fn plain_strings_stay_literal_names_never_paths() {
    let g = nested();
    // v6 has a literal property named `a.b` (9) and a nested a: {b: 7}.
    assert_eq!(list(&g, r#"g.V("6").values("a.b").toList()"#), ["9"]);
    assert_eq!(list(&g, r#"g.V("6").values(jpath("a.b")).toList()"#), ["7"]);
    assert_eq!(
        list(&g, r#"g.V("6").values(jpath("['a.b']")).toList()"#),
        ["9"]
    );
    assert!(list(&g, r#"g.V("1").values("meta.k").toList()"#).is_empty());
    assert_eq!(rust_values(&g, |t| t.v("6").values("a.b")), ["9"]);
    assert_eq!(rust_values(&g, |t| t.v("6").values(jp("a.b"))), ["7"]);
}

#[test]
fn mixed_name_and_path_keys_keep_their_order() {
    let g = nested();
    let value = run_on(
        &g,
        r#"g.V("1").values(["name", jpath("meta.k"), "age"]).toList()"#,
    )
    .unwrap();
    let got: Vec<String> = value
        .into_array()
        .unwrap()
        .iter()
        .map(|v| v.to_string())
        .collect();
    assert_eq!(got, ["marko", "1", "29"]);
    let lock = g.read();
    let got = lock
        .traversal()
        .v("1")
        .values(vec![
            PropKey::from("name"),
            PropKey::from(jp("meta.k")),
            PropKey::from("age"),
        ])
        .to_list()
        .unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got[1].as_i64(), Some(1));
}

#[test]
fn has_with_a_value() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.tags[0]"), "x").values("name").toList()"#
        ),
        ["marko"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k"), 2).values("name").toList()"#
        ),
        ["vadas"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.nested.deep.n"), 5).values("name").toList()"#
        ),
        ["marko"]
    );
    // Type mismatch is "not equal", not an error.
    assert!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k"), "1").values("name").toList()"#
        )
        .is_empty()
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v(None::<()>)
            .has(jp("meta.tags[-1]"), "z")
            .values("name")),
        ["marko"]
    );
    // has(label, key, value) with a path
    assert_eq!(
        list(
            &g,
            r#"g.V().has("person", jpath("meta.k"), 1).values("name").toList()"#
        ),
        ["marko"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has("software", jpath("meta.k"), 1).values("name").toList()"#
        )
        .len(),
        0
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v(None::<()>)
            .has_labeled("person", jp("meta.k"), 2)
            .values("name")),
        ["vadas"]
    );
}

#[test]
fn has_with_predicates() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k"), P.gt(1)).values("name").toList()"#
        ),
        ["vadas"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k"), P.within([1, 2])).values("name").toList()"#
        ),
        ["marko", "vadas"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.tags[0]"), TextP.startingWith("x")).values("name").toList()"#
        ),
        ["marko"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has_p(jpath("meta.k"), P.lt(2)).values("name").toList()"#
        ),
        ["marko"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has("person", jpath("meta.k"), P.gte(2)).values("name").toList()"#
        ),
        ["vadas"]
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v(None::<()>)
            .has_p(jp("meta.k"), P::Gt(1))
            .values("name")),
        ["vadas"]
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v(None::<()>)
            .has_labeled_p("person", jp("meta.k"), P::Lt(2))
            .values("name")),
        ["marko"]
    );
}

#[test]
fn has_exists_and_has_not() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.nested.deep")).values("name").toList()"#
        ),
        ["marko"]
    );
    assert_eq!(
        list(&g, r#"g.V().has(jpath("meta.k")).values("name").toList()"#),
        ["marko", "vadas"]
    );
    assert_eq!(
        list(&g, r#"g.V().hasNot(jpath("meta.k")).count().toList()"#),
        ["4"]
    );
    assert_eq!(
        list(&g, r#"g.V().has_not(jpath("meta.k")).count().toList()"#),
        ["4"]
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v(None::<()>)
            .has_property(jp("meta.tags[2]"))
            .values("name")),
        ["marko"]
    );
    assert_eq!(
        rust_values(&g, |t| t.v(None::<()>).has_not(jp("meta.k")).count()),
        ["4"]
    );
    // The path exists even when it ends on null.
    let _ = run_on(
        &g,
        r#"g.V("3").property_json(#{meta: #{gone: ()}}).toList()"#,
    )
    .unwrap();
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.gone")).values("name").toList()"#
        ),
        ["lop"]
    );
}

#[test]
fn edges_are_read_like_vertices() {
    let g = nested();
    assert_eq!(
        list(&g, r#"g.E().values(jpath("info.rank")).toList()"#),
        ["3"]
    );
    assert_eq!(
        list(&g, r#"g.E().values(jpath("info.list[-1]")).toList()"#),
        ["20"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.E().has(jpath("info.rank"), P.gte(3)).values("weight").toList()"#
        )
        .len(),
        1
    );
    assert_eq!(
        list(&g, r#"g.E().hasNot(jpath("info.rank")).count().toList()"#),
        ["5"]
    );
    assert_eq!(
        rust_values(&g, |t| t
            .e(None::<()>)
            .has(jp("info.list[0]"), 10)
            .values(jp("info.rank"))),
        ["3"]
    );
}

#[test]
fn relative_paths_after_values_read_inside_the_property() {
    let g = nested();
    // `values("meta")` leaves a lazy property reference; a path key then starts inside it.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").values("meta").values(jpath("tags[1]")).toList()"#
        ),
        ["y"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().values("meta").has(jpath("nested.deep.n"), 5).values(jpath("k")).toList()"#
        ),
        ["1"]
    );
    // On a materialized object (a project() result) the path starts at its root.
    assert!(
        list(
            &g,
            r#"g.V("1").project("m").by("meta").values("m").values(jpath("tags[0]")).toList()"#
        )
        .len()
            <= 1
    );
}

#[test]
fn properties_with_a_path_yields_a_path_property_element() {
    let g = nested();
    // key() is the canonical path text, value() the leaf.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.tags[1]")).key().toList()"#
        ),
        ["$.meta.tags[1]"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.tags[1]")).value().toList()"#
        ),
        ["y"]
    );
    assert_eq!(
        list(&g, r#"g.V().properties(jpath("meta.k")).value().toList()"#),
        ["1", "2"]
    );
    // A terminal materializes the value, like a vertex property element.
    assert_eq!(
        list(&g, r#"g.V("1").properties(jpath("meta.k")).toList()"#),
        ["1"]
    );
    // A subtree leaf is the whole subtree; a missing path / mismatch yields nothing.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.tags")).value().toList()"#
        )
        .len(),
        1
    );
    assert!(list(&g, r#"g.V("1").properties(jpath("meta.nope")).toList()"#).is_empty());
    assert!(list(&g, r#"g.V("4").properties(jpath("meta.k")).toList()"#).is_empty());
    // Names and paths mix, in argument order; a name stays a plain property element.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties("name", jpath("meta.k")).key().toList()"#
        ),
        ["$.meta.k", "name"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(["name", jpath("meta.k")]).value().toList()"#
        ),
        ["1", "marko"]
    );
    // Edges.
    assert_eq!(
        list(
            &g,
            r#"g.E("0").properties(jpath("info.list[1]")).value().toList()"#
        ),
        ["20"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.E("0").properties(jpath("info.rank")).key().toList()"#
        ),
        ["$.info.rank"]
    );
    // Rust API.
    let lock = g.read();
    let values = lock
        .traversal()
        .v("1")
        .properties(jp("meta.k"))
        .value()
        .to_list()
        .unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].as_i64(), Some(1));
    let keys = lock
        .traversal()
        .v("1")
        .properties(jp("meta.k"))
        .key()
        .to_list()
        .unwrap();
    assert_eq!(keys[0], ElementProperty::from("$.meta.k"));
}

#[test]
fn path_property_element_behaves_like_a_property_element() {
    let g = nested();
    // label() / hasLabel() / hasKey() see the canonical path text; id() is the same deliberate error.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).label().toList()"#
        ),
        ["$.meta.k"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).hasLabel("$.meta.k").value().toList()"#
        ),
        ["1"]
    );
    assert!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).hasLabel("meta.k").toList()"#
        )
        .is_empty()
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).hasKey("$.meta.k").value().toList()"#
        ),
        ["1"]
    );
    assert!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).hasKey("meta.k").toList()"#
        )
        .is_empty()
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).hasValue(1).key().toList()"#
        ),
        ["$.meta.k"]
    );
    assert!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).hasValue(2).toList()"#
        )
        .is_empty()
    );
    let err =
        run_on(&g, r#"g.V("1").properties(jpath("meta.k")).id().toList()"#).expect_err("no id");
    assert!(err.contains("id"), "{err}");
    // Predicates and casts read the leaf; the string form is the element's toString.
    assert_eq!(
        list(
            &g,
            r#"g.V().properties(jpath("meta.k")).where(P.gt(1)).value().toList()"#
        ),
        ["2"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).as_string().toList()"#
        ),
        ["p[$.meta.k->1]"]
    );
    // dedup() compares key and value; the same path and leaf from two vertices collapse.
    assert_eq!(
        list(
            &g,
            r#"g.V("1","2").properties(jpath("meta.k")).dedup().count().toList()"#
        ),
        ["2"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1","2").properties(jpath("meta.k")).value().dedup().count().toList()"#
        ),
        ["2"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1","2").properties(jpath("meta.k"), jpath("meta.k")).dedup().count().toList()"#
        ),
        ["2"]
    );
    // An element is not a vertex: navigation steps fail with a clear error, not a panic.
    assert!(run_on(&g, r#"g.V("1").properties(jpath("meta.k")).out().toList()"#).is_err());
    assert!(
        run_on(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).has("x").toList()"#
        )
        .is_err()
    );
    // Only a vertex or an edge can have properties.
    assert!(
        run_on(
            &g,
            r#"g.V("1").values("meta").properties(jpath("k")).toList()"#
        )
        .is_err()
    );
    // A plain name is unaffected: no path text, still a stored property handle.
    assert_eq!(
        list(&g, r#"g.V("1").properties("name").label().toList()"#),
        ["name"]
    );
    assert_eq!(
        list(&g, r#"g.V("1").properties("name").key().toList()"#),
        ["name"]
    );
}

#[test]
fn path_property_in_materialization_json_and_display() {
    let g = nested();
    let out = run_on(&g, r#"g.V("1").properties(jpath("meta.k")).toList()"#).unwrap();
    assert_eq!(out.into_array().unwrap()[0].as_int().unwrap(), 1);
    let out = run_on(
        &g,
        r#"g.V("1").properties(jpath("meta.k")).to_json().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(out.contains('1'), "{out}");
    let out = run_on(
        &g,
        r#"g.V("1").properties(jpath("meta.k")).to_table().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(out.contains('1'), "{out}");
    let out = run_on(
        &g,
        r#"g.V("1").properties(jpath("meta.k")).to_table().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(out.contains('1'), "{out}");
    // path() and select() keep the element; a by() on the element reads its leaf.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties(jpath("meta.k")).as("p").select("p").value().toList()"#
        ),
        ["1"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().properties(jpath("meta.k")).order().by(Order.desc).value().toList()"#
        ),
        ["1", "2"]
    );
    let g2 = nested();
    let out = run_on(
        &g2,
        r#"g.V("1").properties(jpath("meta.k")).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(out.contains("properties(jpath(\"$.meta.k\"))"), "{out}");
}

#[test]
fn glob_path_rejects_a_jpath_match_key_clearly() {
    let g = nested();
    let err = run_on(
        &g,
        r#"g.V("1").glob_path("*").by(jpath("meta.k")).toList()"#,
    )
    .expect_err("must fail");
    assert!(err.contains("jpath"), "{err}");
    assert!(err.contains("plain property name"), "{err}");
    assert!(err.contains("by(\"name\")"), "{err}");
    assert!(
        err.contains("reaches into a nested value"),
        "help text missing: {err}"
    );
    // The plain key still works.
    assert!(run_on(&g, r#"g.V("1").glob_path("*").by("name").toList()"#).is_ok());
}

#[test]
fn has_key_stays_a_plain_key_filter() {
    let g = nested();
    // hasKey filters the key of a property element; there is nothing to descend into, so it
    // takes names only. A path string is a literal key: "meta.k" is not the property meta.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").properties("meta").hasKey("meta").key().toList()"#
        ),
        ["meta"]
    );
    assert!(
        list(
            &g,
            r#"g.V("1").properties("meta").hasKey("meta.k").toList()"#
        )
        .is_empty()
    );
}

#[test]
fn works_under_bulk_merging() {
    let g = nested();
    for merge in ["true", "false"] {
        let script = format!(
            r#"g.with("bulk.merge", {merge}).V().both().both().barrier().has(jpath("meta.k"), P.gt(0)).values(jpath("meta.k")).toList()"#
        );
        let on_off = list(&g, &script);
        let plain = list(
            &g,
            &script.replace(
                r#"has(jpath("meta.k"), P.gt(0)).values(jpath("meta.k"))"#,
                r#"has("age").values("age")"#,
            ),
        );
        assert!(!on_off.is_empty());
        assert!(!plain.is_empty());
        let count = format!(
            r#"g.with("bulk.merge", {merge}).V().both().both().barrier().has(jpath("meta.k"), P.gt(0)).count().next()"#
        );
        assert_eq!(
            run_on(&g, &count).unwrap().as_int().unwrap() as usize,
            on_off.len(),
            "{merge}"
        );
    }
    let merged = list(
        &g,
        r#"g.with("bulk.merge", true).V().both().both().barrier().values(jpath("meta.k")).toList()"#,
    );
    let unmerged = list(
        &g,
        r#"g.with("bulk.merge", false).V().both().both().barrier().values(jpath("meta.k")).toList()"#,
    );
    assert_eq!(merged, unmerged);
}

#[test]
fn inside_child_traversals() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V().where(__.has(jpath("meta.k"), 1)).values("name").toList()"#
        ),
        ["marko"]
    );
    assert_eq!(
        list(&g, r#"g.V().not(__.has(jpath("meta.k"))).count().toList()"#),
        ["4"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().filter(__.values(jpath("meta.k")).is(2)).values("name").toList()"#
        ),
        ["vadas"]
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v(None::<()>)
            .where_t(__::has(jp("meta.k"), 1))
            .values("name")),
        ["marko"]
    );
}

#[test]
fn profile_shows_the_path_steps_unfused() {
    let g = nested();
    let lock = g.read();
    let t = lock.traversal();

    // Plain string: unchanged rendering.
    let plain = t
        .v(None::<()>)
        .has("name", "marko")
        .count()
        .profile()
        .unwrap()
        .to_string();
    assert!(plain.contains("has(\"name\", "), "{plain}");

    // A single-segment path is still a path step: never folded into the source or the count.
    let t = lock.traversal();
    let out = t
        .v(None::<()>)
        .has(jp("name"), "marko")
        .count()
        .profile()
        .unwrap()
        .to_string();
    assert!(out.contains("has(jpath(\"$.name\"), "), "{out}");
    assert!(out.contains("count()"), "{out}");
    assert!(!out.contains("count_only"), "{out}");

    // SourceFilterPushdown / HasIdPushdown: the label and id fold, the path filter stays a step.
    let t = lock.traversal();
    let out = t
        .v(None::<()>)
        .has_label("person")
        .has_id("1")
        .has(jp("meta.k"), 1)
        .profile()
        .unwrap()
        .to_string();
    assert!(out.contains("has(jpath(\"$.meta.k\"), "), "{out}");
    assert!(!out.contains("has_label("), "{out}");

    // BarrierPushdown: out_e().has_label(x) folds, the path filter after it stays.
    let t = lock.traversal();
    let out = t
        .v("1")
        .out_e(None::<()>)
        .has_label("knows")
        .has(jp("info.rank"), 3)
        .profile()
        .unwrap()
        .to_string();
    assert!(out.contains("has(jpath(\"$.info.rank\"), "), "{out}");
    assert!(!out.contains("has_label("), "{out}");

    // values(path) and properties(path) render their keys.
    let t = lock.traversal();
    let out = t.v("1").values(jp("meta.k")).profile().unwrap().to_string();
    assert!(out.contains("values(jpath(\"$.meta.k\"))"), "{out}");
    let t = lock.traversal();
    let out = t
        .v("1")
        .has_not(jp("meta.z"))
        .has_property(jp("meta.k"))
        .profile()
        .unwrap()
        .to_string();
    assert!(out.contains("has_not(jpath(\"$.meta.z\"))"), "{out}");
    assert!(out.contains("has(jpath(\"$.meta.k\"))"), "{out}");
}

#[test]
fn path_results_match_the_unfused_equivalent_on_the_large_graph() {
    // Same answer through the path step and through a plain name, with all optimizer rules on.
    let graph = GraphSource::large_110k();
    let lock = graph.read();
    let by_name = lock
        .traversal()
        .v(None::<()>)
        .has("level", "1")
        .count()
        .next()
        .unwrap();
    let by_path = lock
        .traversal()
        .v(None::<()>)
        .has(jp("level"), "1")
        .count()
        .next()
        .unwrap();
    assert_eq!(by_name, by_path);
    assert!(by_name.is_some());
}

#[test]
fn a_path_never_uses_an_index() {
    let graph = GraphSource::tinkerpop_modern();
    let id_before = graph
        .read()
        .traversal()
        .v(None::<()>)
        .has("name", "marko")
        .values("age")
        .to_list()
        .unwrap();
    let lock = graph.read();
    let via_path = lock
        .traversal()
        .v(None::<()>)
        .has(jp("name"), "marko")
        .values("age")
        .to_list()
        .unwrap();
    assert_eq!(id_before, via_path);
    let stats = lock
        .traversal()
        .v(None::<()>)
        .has(jp("name"), "marko")
        .profile()
        .unwrap()
        .to_string();
    assert!(stats.contains("has(jpath("), "{stats}");
}

#[test]
fn rhai_rejects_nothing_new_for_plain_calls() {
    // The widened `has` registration still routes tokens and 3-argument forms.
    let g = nested();
    assert_eq!(
        list(&g, r#"g.V().has(T.label, "person").count().toList()"#),
        ["4"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has("person", "name", "marko").values("age").toList()"#
        ),
        ["29"]
    );
    assert_eq!(
        list(&g, r#"g.V().has("age", P.gt(30)).values("name").toList()"#),
        ["josh", "peter"]
    );
    assert_eq!(list(&g, r#"g.V().has("name").count().toList()"#), ["6"]);
}

// ---------------------------------------------------------------------------------------------
// Task 6: `valueMap` / `elementMap` key arguments and every `by()` consumer accept a path.
// ---------------------------------------------------------------------------------------------

/// The elements of a `toList()` result in result order (no sorting).
fn ordered(graph: &Arc<Graph>, script: &str) -> Vec<String> {
    let value = run_on(graph, script).unwrap_or_else(|e| panic!("{script}: {e}"));
    let array = value
        .into_array()
        .unwrap_or_else(|_| panic!("{script}: not an array"));
    array.iter().map(|v| v.to_string()).collect()
}

fn text_on(graph: &Arc<Graph>, script: &str) -> String {
    run_on(graph, script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
        .to_string()
}

fn rust_json(
    graph: &Arc<Graph>,
    build: impl for<'a> FnOnce(
        GraphTraversalSource<'a, &'a TraversalGraph>,
    ) -> GraphTraversalSource<'a, &'a TraversalGraph>,
) -> Vec<ElementProperty> {
    let lock = graph.read();
    build(lock.traversal()).to_list().expect("traversal")
}

#[test]
fn value_map_and_element_map_take_path_keys() {
    let g = nested();
    // The result key of a path is its canonical text; the value stays list-wrapped for valueMap.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").valueMap(jpath("meta.tags[0]"), "name").toList()"#
        ),
        [r#"#{"$.meta.tags[0]": ["x"], "name": ["marko"]}"#]
    );
    assert_eq!(
        list(&g, r#"g.v("1").value_map([jpath("meta.k")]).toList()"#),
        [r#"#{"$.meta.k": [1]}"#]
    );
    // valueMap(true, ..) keeps the tokens in front.
    assert_eq!(
        list(&g, r#"g.V("2").valueMap(true, jpath("meta.k")).toList()"#),
        [r#"#{"$.meta.k": [2], "id": "2", "label": "person"}"#]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").elementMap(jpath("meta.nested.deep.n"), "name").toList()"#
        ),
        [r#"#{"$.meta.nested.deep.n": 5, "id": "1", "label": "person", "name": "marko"}"#]
    );
    assert_eq!(
        list(
            &g,
            r#"g.v("1").element_map(jpath("meta.items[1].n")).toList()"#
        ),
        [r#"#{"$.meta.items[1].n": 2, "id": "1", "label": "person"}"#]
    );
    // Negative index, bracket names, edges.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").elementMap(jpath("meta.tags[-1]")).toList()"#
        ),
        [r#"#{"$.meta.tags[-1]": "z", "id": "1", "label": "person"}"#]
    );
    assert!(
        list(&g, r#"g.E("0").elementMap(jpath("info.list[1]")).toList()"#)[0]
            .contains(r#""$.info.list[1]": 20"#)
    );
    assert_eq!(
        list(&g, r#"g.E("0").valueMap(jpath("info.rank")).toList()"#),
        [r#"#{"$.info.rank": [3]}"#]
    );
}

#[test]
fn value_map_and_element_map_missing_paths_are_absent() {
    let g = nested();
    // missing key, out of range, descent through a scalar, key on an array: no entry, no error.
    for path in [
        "meta.nope",
        "meta.tags[9]",
        "meta.k.x",
        "meta.tags.x",
        "nope",
    ] {
        assert_eq!(
            list(
                &g,
                &format!(r#"g.V("1").valueMap(jpath("{path}")).toList()"#)
            ),
            ["#{}"],
            "{path}"
        );
        assert_eq!(
            list(
                &g,
                &format!(r#"g.V("1").elementMap(jpath("{path}")).toList()"#)
            ),
            [r#"#{"id": "1", "label": "person"}"#],
            "{path}"
        );
    }
    // A scalar property below which nothing exists, and an element without the property at all.
    assert_eq!(
        list(&g, r#"g.V("4").valueMap(jpath("meta.k")).toList()"#),
        ["#{}"]
    );
    assert_eq!(
        list(&g, r#"g.V("3").valueMap(jpath("meta.k")).toList()"#),
        ["#{}"]
    );
}

#[test]
fn value_map_plain_keys_and_literal_dotted_names_are_unchanged() {
    let g = nested();
    // "a.b" is the literal property name; jpath("a.b") is the nested path; both can be asked for.
    assert_eq!(
        list(&g, r#"g.V("6").valueMap("a.b").toList()"#),
        [r#"#{"a.b": [9]}"#]
    );
    assert_eq!(
        list(&g, r#"g.V("6").valueMap(jpath("a.b")).toList()"#),
        [r#"#{"$.a.b": [7]}"#]
    );
    assert_eq!(
        list(&g, r#"g.V("6").valueMap("a.b", jpath("a.b")).toList()"#),
        [r#"#{"$.a.b": [7], "a.b": [9]}"#]
    );
}

#[test]
fn value_map_by_applies_to_path_entries() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V("1").valueMap(jpath("meta.tags[0]"), "name").by(__.unfold()).toList()"#
        ),
        [r#"#{"$.meta.tags[0]": "x", "name": "marko"}"#]
    );
}

#[test]
fn value_map_and_element_map_rust_api() {
    let g = nested();
    let maps = rust_json(&g, |t| t.v("1").value_map(jp("meta.tags[1]")));
    match &maps[0] {
        ElementProperty::Object(obj) => {
            assert_eq!(
                obj.get("$.meta.tags[1]"),
                Some(&ElementProperty::Array(vec![ElementProperty::from("y")]))
            );
            assert_eq!(obj.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    let maps = rust_json(&g, |t| {
        t.v("1")
            .element_map(vec![PropKey::from("name"), PropKey::from(jp("meta.k"))])
    });
    match &maps[0] {
        ElementProperty::Object(obj) => {
            assert_eq!(obj.get("name"), Some(&ElementProperty::from("marko")));
            assert_eq!(obj.get("$.meta.k").and_then(|v| v.as_i64()), Some(1));
        }
        other => panic!("{other:?}"),
    }
    let maps = rust_json(&g, |t| t.v("2").value_map_with_tokens(jp("meta.k")));
    match &maps[0] {
        ElementProperty::Object(obj) => {
            assert!(obj.get("id").is_some() && obj.get("label").is_some());
            assert!(obj.get("$.meta.k").is_some());
        }
        other => panic!("{other:?}"),
    }
    // Existing plain callers still compile and behave.
    assert_eq!(rust_json(&g, |t| t.v("1").value_map(None)).len(), 1);
    assert_eq!(
        rust_json(&g, |t| t.v("1").element_map(vec!["name"])).len(),
        1
    );
}

#[test]
fn element_map_with_a_path_on_a_map_value() {
    let g = nested();
    // elementMap on an already materialized map (a Record) reads the path inside it.
    assert_eq!(
        list(
            &g,
            r#"g.V("1").elementMap().elementMap(jpath("meta.k")).toList()"#
        ),
        [r#"#{"$.meta.k": 1, "id": "1", "label": "person"}"#]
    );
}

#[test]
fn profile_of_value_map_and_element_map() {
    let g = nested();
    let text = |script: &str| run_on(&g, script).unwrap().to_string();
    let p = text(r#"g.V("1").valueMap(jpath("meta.k"), "name").profile().to_string()"#);
    assert!(
        p.contains(r#"value_map("name", jpath("$.meta.k"))"#)
            || p.contains(r#"value_map(jpath("$.meta.k"), "name")"#),
        "{p}"
    );
    let p = text(r#"g.V("1").elementMap(jpath("meta.k")).profile().to_string()"#);
    assert!(p.contains(r#"element_map(jpath("$.meta.k"))"#), "{p}");
    // Plain names render exactly as before.
    let p = text(r#"g.V("1").elementMap("name").profile().to_string()"#);
    assert!(p.contains(r#"element_map("name")"#), "{p}");
}

// ---- by() consumers --------------------------------------------------------------------------

#[test]
fn order_by_a_path() {
    let g = nested();
    // vertices without the path are dropped, exactly like a missing plain property.
    assert_eq!(
        ordered(
            &g,
            r#"g.V().order().by(jpath("meta.k")).values("name").toList()"#
        ),
        ["marko", "vadas"]
    );
    assert_eq!(
        ordered(
            &g,
            r#"g.V().order().by(jpath("meta.k"), Order.desc).values("name").toList()"#
        ),
        ["vadas", "marko"]
    );
    assert_eq!(
        ordered(
            &g,
            r#"g.v().order().by(jpath("meta.tags[-1]")).values("name").toList()"#
        ),
        ["vadas", "marko"]
    );
    assert_eq!(
        ordered(
            &g,
            r#"g.V().order().by(jpath("meta.items[0].n")).values("name").toList()"#
        ),
        ["marko"]
    );
    // mixed name and path in one order()
    assert_eq!(
        ordered(
            &g,
            r#"g.V().has("age").order().by(jpath("meta.k")).by("name").values("name").toList()"#
        ),
        ["marko", "vadas"]
    );
    // the same through order(Scope.local)
    assert_eq!(
        ordered(
            &g,
            r#"g.V().has(jpath("meta.k")).fold().order(Scope.local).by(jpath("meta.k"), Order.desc).unfold().values("name").toList()"#
        ),
        ["vadas", "marko"]
    );
    // a path equal to the plain name orders like it
    assert_eq!(
        ordered(
            &g,
            r#"g.V().order().by(jpath("age")).values("name").toList()"#
        ),
        ordered(&g, r#"g.V().order().by("age").values("name").toList()"#)
    );
}

/// A map result as `key=value` pairs sorted by key text (script maps keep insertion order).
fn map_pairs(graph: &Arc<Graph>, script: &str) -> Vec<String> {
    let script = format!(
        "let m = {script}; let out = []; for k in m.keys() {{ out.push(`${{k}}=${{m[k]}}`); }} out"
    );
    list(graph, &script)
}

#[test]
fn group_and_group_count_by_a_path() {
    let g = nested();
    assert_eq!(
        map_pairs(&g, r#"g.V().groupCount().by(jpath("meta.k")).next()"#),
        ["1=1", "2=1"]
    );
    assert_eq!(
        map_pairs(
            &g,
            r#"g.V().group_count().by(jpath("meta.tags[0]")).next()"#
        ),
        ["q=1", "x=1"]
    );
    assert_eq!(
        map_pairs(&g, r#"g.V().group().by(jpath("meta.k")).by("name").next()"#),
        [r#"1=["marko"]"#, r#"2=["vadas"]"#]
    );
    // path as the value projection: a missing path leaves the group's list empty, exactly like
    // a missing plain property
    assert_eq!(
        map_pairs(
            &g,
            r#"g.V().has("age").group().by("name").by(jpath("meta.k")).next()"#
        ),
        ["josh=[]", "marko=[1]", "peter=[]", "vadas=[2]"]
    );
    assert_eq!(
        map_pairs(
            &g,
            r#"g.V().has("age").group().by("name").by("zzz").next()"#
        ),
        ["josh=[]", "marko=[]", "peter=[]", "vadas=[]"]
    );
    // group().by(path) keys keep their type (integers stay integers)
    assert_eq!(
        run_on(
            &g,
            r#"let m = g.V().groupCount().by(jpath("meta.k")).next(); m[1] + m[2]"#
        )
        .unwrap()
        .as_int()
        .unwrap(),
        2
    );
}

#[test]
fn group_count_by_a_path_is_never_fused() {
    let g = nested();
    let p = run_on(
        &g,
        r#"g.V().groupCount().by(jpath("meta.k")).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(p.contains(r#"group_count().by(jpath("$.meta.k"))"#), "{p}");
    assert!(p.contains("v()"), "{p}");
    assert!(!p.contains("group_count_pushdown"), "{p}");
    let p = run_on(
        &g,
        r#"g.V().hasLabel("person").groupCount().by(jpath("label")).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(!p.contains("group_count_pushdown"), "{p}");
}

#[test]
fn dedup_by_a_path() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V().dedup().by(jpath("meta.k")).values("name").toList()"#
        ),
        ["marko", "vadas"]
    );
    // two vertices share the same value: only the first stays
    assert_eq!(
        list(
            &g,
            r#"g.V().has("age").dedup().by(jpath("meta.tags.x")).toList()"#
        )
        .len(),
        0
    );
    assert_eq!(
        run_on(&g, r#"g.V().dedup().by(jpath("meta.k")).count().next()"#)
            .unwrap()
            .as_int()
            .unwrap(),
        2
    );
}

#[test]
fn path_tree_select_project_by_a_path() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V("1").out("knows").path().by(jpath("meta.k")).toList()"#
        ),
        ["[1, 2]"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").out("knows").path().by(jpath("meta.k")).by("name").toList()"#
        ),
        [
            r#"[1, "josh""#.to_string() + "]",
            r#"[1, "vadas"]"#.to_string()
        ]
    );
    assert_eq!(
        run_on(
            &g,
            r#"g.V("1").out("knows").tree().by(jpath("meta.k")).next().to_string()"#
        )
        .unwrap()
        .to_string(),
        "{1: {2: #{}}}"
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k")).as("a").select("a").by(jpath("meta.k")).toList()"#
        ),
        ["1", "2"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("1").as("a").out("knows").as("b").select("a", "b").by(jpath("meta.k")).toList()"#
        ),
        [r#"#{"a": 1, "b": 2}"#]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k")).project("k", "t", "n").by(jpath("meta.k")).by(jpath("meta.tags[0]")).by("name").toList()"#
        ),
        [
            r#"#{"k": 1, "n": "marko", "t": "x"}"#,
            r#"#{"k": 2, "n": "vadas", "t": "q"}"#
        ]
    );
    // a missing path is an unproductive by(): same outcome as a missing plain property
    assert_eq!(
        list(&g, r#"g.V().project("k").by(jpath("meta.nope")).toList()"#),
        list(&g, r#"g.V().project("k").by("nope").toList()"#)
    );
    assert_eq!(
        list(&g, r#"g.V().project("k").by(jpath("meta.k")).toList()"#)
            .iter()
            .filter(|m| m.as_str() == "#{}")
            .count(),
        4
    );
}

#[test]
fn where_math_aggregate_store_sack_by_a_path() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V("2").as("a").in("knows").as("b").where(P.gt("a")).by(jpath("meta.k")).values("name").toList()"#
        ),
        Vec::<String>::new(),
        "1 > 2 is false"
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("2").as("a").in("knows").as("b").where(P.lt("a")).by(jpath("meta.k")).values("name").toList()"#
        ),
        ["marko"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k")).math("_ * 10").by(jpath("meta.k")).toList()"#
        ),
        ["10.0", "20.0"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k")).aggregate("x").by(jpath("meta.k")).cap("x").next()"#
        ),
        list(&g, r#"[1, 2]"#)
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k")).store("x").by(jpath("meta.tags[0]")).cap("x").next()"#
        ),
        ["q", "x"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.withSack(0).V().has(jpath("meta.k")).sack(Operator.sum).by(jpath("meta.k")).sack().toList()"#
        ),
        ["1", "2"]
    );
}

#[test]
fn by_a_path_on_edges_and_with_type_mismatches() {
    let g = nested();
    assert_eq!(
        text_on(
            &g,
            r#"g.E().has(jpath("info.rank")).group().by(jpath("info.rank")).by(jpath("info.list[0]")).next().to_string()"#
        ),
        "{3: [10]}"
    );
    assert_eq!(list(&g, r#"g.E().order().by(jpath("info.list[-1]")).by(jpath("info.rank")).elementMap(jpath("info.rank")).toList()"#).len(), 1);
    // v4's meta is a scalar string: any path below it is absent, never an error
    assert_eq!(
        list(&g, r#"g.V("4").project("k").by(jpath("meta.k")).toList()"#),
        ["#{}"]
    );
    assert_eq!(
        list(&g, r#"g.V("4").order().by(jpath("meta.k")).toList()"#),
        Vec::<String>::new()
    );
    assert_eq!(
        list(&g, r#"g.V("4").dedup().by(jpath("meta[0]")).toList()"#),
        Vec::<String>::new()
    );
    assert_eq!(
        list(
            &g,
            r#"g.V("4").groupCount().by(jpath("meta.tags")).toList()"#
        )
        .len(),
        1
    );
}

#[test]
fn by_a_path_in_child_traversals_and_anonymous_steps() {
    let g = nested();
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k")).where(__.project("k").by(jpath("meta.k")).select("k").is(P.gt(1))).values("name").toList()"#
        ),
        ["vadas"]
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k")).local(__.elementMap(jpath("meta.k"))).toList()"#
        )
        .len(),
        2
    );
    assert_eq!(
        list(
            &g,
            r#"g.V().has(jpath("meta.k")).union(__.valueMap(jpath("meta.k"))).toList()"#
        ),
        [r#"#{"$.meta.k": [1]}"#, r#"#{"$.meta.k": [2]}"#]
    );
}

#[test]
fn by_a_path_rust_api() {
    let g = nested();
    // by(JsonPath), by(&JsonPath) and by(PropKey)
    assert_eq!(
        rust_values(&g, |t| t
            .v(None)
            .order()
            .by(jp("meta.k"))
            .by(graphersal::prelude::Order::Desc)
            .values("name")),
        ["marko", "vadas"]
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v(None)
            .order()
            .by(PropKey::from(jp("meta.k")))
            .values("name")),
        ["marko", "vadas"]
    );
    assert_eq!(
        rust_values(&g, |t| t.v(None).dedup().by(jp("meta.k")).values("name")),
        ["marko", "vadas"]
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v(None)
            .has_property(jp("meta.k"))
            .project(vec!["k"])
            .by(jp("meta.k"))
            .select("k")),
        ["1", "2"]
    );
    // PropKey::Name keeps the string meaning: a property key, never the label/id token
    assert_eq!(
        rust_values(&g, |t| t
            .v("1")
            .order()
            .by(PropKey::from("name"))
            .values("name")),
        ["marko"]
    );
    assert_eq!(
        rust_values(&g, |t| t
            .v("1")
            .order()
            .by(PropKey::from("label"))
            .values("name")),
        Vec::<String>::new()
    );
    let counts = rust_json(&g, |t| t.v(None).group_count().by(jp("meta.k")));
    match &counts[0] {
        ElementProperty::Map(m) => assert_eq!(m.len(), 2),
        ElementProperty::Object(m) => assert_eq!(m.len(), 2),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        rust_values(&g, |t| t
            .v("2")
            .as_("a")
            .in_("knows")
            .as_("b")
            .where_p(P::Lt("a"))
            .by(jp("meta.k"))
            .values("name")),
        ["marko"]
    );
}

#[test]
fn by_a_path_under_bulk_merging_on_and_off() {
    let g = nested();
    for q in [
        r#"V().has(jpath("meta.k")).out().groupCount().by(jpath("meta.k"))"#,
        r#"V().both().both().group().by(jpath("meta.k")).by(jpath("meta.tags[0]"))"#,
        r#"V().both().both().dedup().by(jpath("meta.k")).values("name")"#,
        r#"V().both().barrier().both().order().by(jpath("meta.k")).by("name").values("name")"#,
        r#"V().both().both().project("k").by(jpath("meta.k"))"#,
        r#"V().both().barrier().valueMap(jpath("meta.k"))"#,
        r#"V().both().both().elementMap(jpath("meta.tags[0]"), "name")"#,
        r#"V().both().barrier().path().by(jpath("meta.k"))"#,
    ] {
        let tail = if q.contains("groupCount") || q.contains(".group()") {
            ".next().to_string()"
        } else {
            ".toList()"
        };
        let on = run_on(&g, &format!("g.{q}{tail}"))
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .to_string();
        let off = run_on(&g, &format!(r#"g.with("bulk.merge", false).{q}{tail}"#))
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .to_string();
        if tail == ".toList()" {
            assert_eq!(
                list(&g, &format!("g.{q}.toList()")),
                list(&g, &format!(r#"g.with("bulk.merge", false).{q}.toList()"#)),
                "{q}"
            );
        } else {
            assert_eq!(on, off, "{q}");
        }
    }
}

#[test]
fn single_segment_path_by_equals_the_plain_name_everywhere() {
    let g = nested();
    for (path, plain) in [
        (
            r#"order().by(jpath("age")).values("name")"#,
            r#"order().by("age").values("name")"#,
        ),
        (
            r#"dedup().by(jpath("age")).values("name")"#,
            r#"dedup().by("age").values("name")"#,
        ),
        (
            r#"project("a").by(jpath("age"))"#,
            r#"project("a").by("age")"#,
        ),
        (
            r#"valueMap(jpath("age")).unfold()"#,
            r#"valueMap("age").unfold().map(__.identity())"#,
        ),
        (r#"path().by(jpath("name"))"#, r#"path().by("name")"#),
    ] {
        if path.starts_with("valueMap") {
            continue;
        }
        assert_eq!(
            list(&g, &format!("g.V().{path}.toList()")),
            list(&g, &format!("g.V().{plain}.toList()")),
            "{path}"
        );
    }
}

#[test]
fn profile_shows_by_paths_readably() {
    let g = nested();
    let p = run_on(
        &g,
        r#"g.V().order().by(jpath("meta.k")).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(p.contains(r#"jpath("$.meta.k")"#), "{p}");
    let p = run_on(&g, r#"g.V().order().by("age").profile().to_string()"#)
        .unwrap()
        .to_string();
    assert!(p.contains(r#"order().by("age")"#), "{p}");
}

// ---------------------------------------------------------------------------------------------
// Task 8: write side, `property(jpath(..), value)`.
// ---------------------------------------------------------------------------------------------

fn fresh() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::empty());
    let _ = run_on(
        &graph,
        r#"
        g.addV("p").property(T.id, "a").property_json(#{meta: #{tags: ["x", "y"], k: 1, o: #{deep: #{n: 5}}}}).toList();
        g.addV("p").property(T.id, "b").property("scalar", 5).toList();
        g.addV("p").property(T.id, "c").toList();
        g.V("a").addE("e").to(__.V("b")).property(T.id, "e1").property("info", #{rank: 3, list: [10]}).toList();
        1
        "#,
    )
    .expect("fixture");
    graph
}

fn json_of(graph: &Arc<Graph>, id: &str, prop: &str) -> String {
    run_on(graph, &format!(r#"g.V("{id}").values("{prop}").toList()"#))
        .unwrap_or_else(|e| panic!("{e}"))
        .into_array()
        .expect("array")
        .iter()
        .map(|v| format!("{v:?}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn err_of(graph: &Arc<Graph>, script: &str) -> String {
    run_on(graph, script).expect_err("write must fail")
}

#[test]
fn write_creates_nested_objects_for_key_segments() {
    let g = fresh();
    // The property does not exist: key segments create the whole chain.
    let _ = run_on(
        &g,
        r#"g.V("c").property(jpath("cfg.net.ip"), "10.0.0.1").toList()"#,
    )
    .unwrap();
    assert_eq!(
        list(&g, r#"g.V("c").values(jpath("cfg.net.ip")).toList()"#),
        ["10.0.0.1"]
    );
    // An existing object gains a missing key at any depth, siblings stay.
    let _ = run_on(&g, r#"g.V("a").property(jpath("meta.o.deep.m"), 6).property(jpath("meta.fresh.x"), true).toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.o.deep.m")).toList()"#),
        ["6"]
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.o.deep.n")).toList()"#),
        ["5"]
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.fresh.x")).toList()"#),
        ["true"]
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.k")).toList()"#),
        ["1"]
    );
}

#[test]
fn write_replaces_a_leaf_and_a_whole_subtree() {
    let g = fresh();
    let _ = run_on(&g, r#"g.V("a").property(jpath("meta.k"), 99).property(jpath("meta.o"), #{z: [1, 2]}).toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.k")).toList()"#),
        ["99"]
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.o.z[1]")).toList()"#),
        ["2"]
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.o.deep")).toList()"#),
        Vec::<String>::new()
    );
    // A new type at the same path is a replace, not an error.
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.k"), "now text").toList()"#,
    )
    .unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.k")).toList()"#),
        ["now text"]
    );
    // A single-segment path is the plain property.
    let _ = run_on(&g, r#"g.V("a").property(jpath("plain"), 1).toList()"#).unwrap();
    assert_eq!(list(&g, r#"g.V("a").values("plain").toList()"#), ["1"]);
}

#[test]
fn write_index_replaces_appends_and_counts_from_the_end() {
    let g = fresh();
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.tags[0]"), "X").toList()"#,
    )
    .unwrap();
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.tags[2]"), "z").toList()"#,
    )
    .unwrap(); // [len] appends
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.tags[-1]"), "Z").toList()"#,
    )
    .unwrap();
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.tags[-3]"), "first").toList()"#,
    )
    .unwrap();
    assert_eq!(
        run_on(&g, r#"g.V("a").values(jpath("meta.tags[0]"), jpath("meta.tags[1]"), jpath("meta.tags[2]")).toList()"#)
            .unwrap()
            .into_array()
            .unwrap()
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>(),
        ["first", "y", "Z"]
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[3]")).toList()"#),
        Vec::<String>::new()
    );
    // An element of an array can be an object that is written below.
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.items[0]"), 1).toList()"#,
    )
    .unwrap_err();
    let _ = run_on(&g, r#"g.V("a").property(jpath("meta.items"), [#{n: 1}]).property(jpath("meta.items[0].n"), 2).toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.items[0].n")).toList()"#),
        ["2"]
    );
    // Appending an object element, then writing below it, needs two steps ([len] appends only
    // as the last segment).
    let _ = run_on(&g, r#"g.V("a").property(jpath("meta.items[1]"), #{n: 3}).property(jpath("meta.items[1].n"), 4).toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.items[1].n")).toList()"#),
        ["4"]
    );
}

#[test]
fn write_errors_leave_the_property_untouched() {
    let g = fresh();
    let before = json_of(&g, "a", "meta");
    let cases = [
        // index > len: no null padding
        (
            r#"property(jpath("meta.tags[3]"), "x")"#,
            "index 3 is out of range for the array '$.meta.tags' of length 2",
        ),
        (
            r#"property(jpath("meta.tags[-3]"), "x")"#,
            "index -3 is out of range for the array '$.meta.tags' of length 2",
        ),
        // [len] with something below it
        (
            r#"property(jpath("meta.tags[2].n"), "x")"#,
            "index 2 is out of range",
        ),
        // key on an array / index on an object / descent through a scalar
        (
            r#"property(jpath("meta.tags.x"), "x")"#,
            "'$.meta.tags' is an array, not an object",
        ),
        (
            r#"property(jpath("meta.o[0]"), "x")"#,
            "'$.meta.o' is an object, not an array",
        ),
        (
            r#"property(jpath("meta.k.x"), "x")"#,
            "'$.meta.k' is a number, not an object",
        ),
        (
            r#"property(jpath("meta.k[0]"), "x")"#,
            "'$.meta.k' is a number, not an array",
        ),
        // a missing container is created for keys only
        (
            r#"property(jpath("meta.missing[0]"), "x")"#,
            "the array '$.meta.missing' does not exist",
        ),
        (
            r#"property(jpath("meta.missing.deeper[0]"), "x")"#,
            "the array '$.meta.missing.deeper' does not exist",
        ),
        (
            r#"property(jpath("nothere[0]"), "x")"#,
            "the array '$.nothere' does not exist",
        ),
        (
            r#"property(jpath("[0]"), "x")"#,
            "the first segment must name the property",
        ),
    ];
    for (step, message) in cases {
        let err = err_of(&g, &format!(r#"g.V("a").{step}.toList()"#));
        assert!(err.contains(message), "{step}: {err}");
        assert!(err.contains("Cannot write property path"), "{step}: {err}");
        assert_eq!(
            json_of(&g, "a", "meta"),
            before,
            "{step} must not change the property"
        );
    }
    // descent through null
    let _ = run_on(&g, r#"g.V("a").property(jpath("meta.nul"), ()).toList()"#).unwrap();
    let err = err_of(&g, r#"g.V("a").property(jpath("meta.nul.x"), 1).toList()"#);
    assert!(
        err.contains("'$.meta.nul' is a null, not an object") || err.contains("is null"),
        "{err}"
    );
    // A missing leaf-less top level with an index
    assert_eq!(json_of(&g, "c", "nothere"), "");
    // empty array: negative index and [0] append
    let _ = run_on(&g, r#"g.V("a").property(jpath("meta.empty"), []).toList()"#).unwrap();
    let err = err_of(
        &g,
        r#"g.V("a").property(jpath("meta.empty[-1]"), 1).toList()"#,
    );
    assert!(
        err.contains("out of range for the array '$.meta.empty' of length 0"),
        "{err}"
    );
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.empty[0]"), 1).toList()"#,
    )
    .unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.empty[0]")).toList()"#),
        ["1"]
    );
}

#[test]
fn write_to_a_scalar_property_is_an_error_not_an_overwrite() {
    let g = fresh();
    let err = err_of(&g, r#"g.V("b").property(jpath("scalar.x"), 1).toList()"#);
    assert!(
        err.contains("'$.scalar' is a number, not an object"),
        "{err}"
    );
    assert_eq!(list(&g, r#"g.V("b").values("scalar").toList()"#), ["5"]);
}

#[test]
fn write_on_edges() {
    let g = fresh();
    let _ = run_on(&g, r#"g.E("e1").property(jpath("info.rank"), 9).property(jpath("info.list[1]"), 20).property(jpath("info.note.t"), "n").toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.E("e1").values(jpath("info.rank")).toList()"#),
        ["9"]
    );
    assert_eq!(
        list(&g, r#"g.E("e1").values(jpath("info.list[1]")).toList()"#),
        ["20"]
    );
    assert_eq!(
        list(&g, r#"g.E("e1").values(jpath("info.note.t")).toList()"#),
        ["n"]
    );
    // an edge without any properties gets the property created
    let _ = run_on(&g, r#"g.V("b").addE("e").to(__.V("c")).property(T.id, "e2").property(jpath("a.b"), 1).toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.E("e2").values(jpath("a.b")).toList()"#),
        ["1"]
    );
    let err = err_of(
        &g,
        r#"g.E("e1").property(jpath("info.list.x"), 1).toList()"#,
    );
    assert!(err.contains("'$.info.list' is an array"), "{err}");
}

#[test]
fn write_in_an_add_chain_and_with_a_traversal_value() {
    let g = fresh();
    let _ = run_on(&g, r#"g.addV("q").property(T.id, "n1").property(jpath("meta.name"), "m").property(jpath("meta.deep.x"), 1).toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V("n1").values(jpath("meta.name")).toList()"#),
        ["m"]
    );
    // value from a traversal (property_by form)
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.copy"), __.values(jpath("meta.k"))).toList()"#,
    )
    .unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.copy")).toList()"#),
        ["1"]
    );
    // an empty sub-traversal leaves everything untouched, like the plain form
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.none"), __.values("nope")).toList()"#,
    )
    .unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.none")).toList()"#),
        Vec::<String>::new()
    );
    // nested objects and arrays as the value, then read back through the top-level property
    let _ = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.blob"), #{p: [1, #{q: 2}]}).toList()"#,
    )
    .unwrap();
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.blob.p[1].q")).toList()"#),
        ["2"]
    );
    let top = run_on(&g, r#"g.V("a").values("meta").next().blob.p[1].q"#).unwrap();
    assert_eq!(top.as_int().unwrap(), 2);
}

#[test]
fn write_value_may_not_be_a_vertex_or_an_edge() {
    let g = fresh();
    let err = err_of(
        &g,
        r#"g.V("a").property(jpath("meta.friend"), __.out()).toList()"#,
    );
    assert!(err.contains("cannot hold a vertex"), "{err}");
    let err = err_of(
        &g,
        r#"g.V("a").property(jpath("meta.via"), __.outE()).toList()"#,
    );
    assert!(err.contains("cannot hold an edge"), "{err}");
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.friend")).toList()"#),
        Vec::<String>::new()
    );
}

#[test]
fn write_with_a_cardinality_is_an_explicit_error() {
    let g = fresh();
    for script in [
        r#"g.V("a").property(Cardinality.single, jpath("meta.k"), 2).toList()"#,
        r#"g.V("a").property(Cardinality.list, jpath("meta.k"), 2).toList()"#,
        r#"g.V("a").property(Cardinality.single, jpath("meta.k"), __.constant(2)).toList()"#,
    ] {
        let err = err_of(&g, script);
        assert!(
            err.contains("property(Cardinality, jpath(..), value) is not supported"),
            "{script}: {err}"
        );
        assert!(
            err.contains("Graphersal stores one value per property"),
            "{script}: {err}"
        );
    }
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.k")).toList()"#),
        ["1"]
    );
}

#[test]
fn write_key_must_be_a_string_or_a_path() {
    let g = fresh();
    let err = err_of(&g, r#"g.V("a").property(5, 1).toList()"#);
    assert!(
        err.contains("expected a property name string or a jpath(..)"),
        "{err}"
    );
    // a plain string with dots is a literal property name
    let _ = run_on(&g, r#"g.V("a").property("x.y", 1).toList()"#).unwrap();
    assert_eq!(list(&g, r#"g.V("a").values("x.y").toList()"#), ["1"]);
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("x.y")).toList()"#),
        Vec::<String>::new()
    );
}

#[test]
fn write_interplays_with_property_json_and_plain_reads() {
    let g = fresh();
    let _ = run_on(&g, r#"g.V("c").property_json(#{cfg: #{a: [1]}}).property(jpath("cfg.a[1]"), 2).property(jpath("cfg.b"), "s").toList()"#).unwrap();
    // read through the whole top-level object
    let cfg = run_on(&g, r#"g.V("c").values("cfg").next()"#)
        .unwrap()
        .cast::<rhai::Map>();
    assert_eq!(cfg["b"].to_string(), "s");
    assert_eq!(cfg["a"].clone().into_array().unwrap().len(), 2);
    // property_json merging a top-level key afterwards replaces the whole property
    let _ = run_on(&g, r#"g.V("c").property_json(#{cfg: #{z: 1}}).toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V("c").values(jpath("cfg.z")).toList()"#),
        ["1"]
    );
    assert_eq!(
        list(&g, r#"g.V("c").values(jpath("cfg.b")).toList()"#),
        Vec::<String>::new()
    );
    // has() before/after (no property index exists: reads always see the stored value)
    assert_eq!(list(&g, r#"g.V().has("cfg").count().toList()"#), ["1"]);
    let _ = run_on(&g, r#"g.V("c").property(jpath("cfg.z"), 7).toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V().has(jpath("cfg.z"), 1).count().toList()"#),
        ["0"]
    );
    assert_eq!(
        list(&g, r#"g.V().has(jpath("cfg.z"), 7).count().toList()"#),
        ["1"]
    );
    // single-segment path write on an indexed-looking property: has("name", ..) sees it at once
    let _ = run_on(&g, r#"g.V("c").property(jpath("tag"), "t1").toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V().has("tag", "t1").count().toList()"#),
        ["1"]
    );
    let _ = run_on(&g, r#"g.V("c").property(jpath("tag"), "t2").toList()"#).unwrap();
    assert_eq!(
        list(&g, r#"g.V().has("tag", "t1").count().toList()"#),
        ["0"]
    );
    assert_eq!(
        list(&g, r#"g.V().has("tag", "t2").count().toList()"#),
        ["1"]
    );
}

#[test]
fn write_many_traversers_and_bulk_merging_agree() {
    let state = |merge: bool| {
        let g = fresh();
        let _ = run_on(&g, r#"g.addV("p").property(T.id, "d").toList(); g.V("a").addE("e").to(__.V("d")).toList(); g.V("d").addE("e").to(__.V("a")).toList(); 1"#).unwrap();
        // "a" is reached through two different paths: the write runs once per traverser, and a
        // replace is idempotent; the append case below counts the executions.
        let _ = run_on(&g, &format!(r#"g.with("bulk.merge", {merge}).V("a").both().both().barrier().property(jpath("meta.seen[0]"), 1).toList()"#)).unwrap_err();
        let _ = run_on(&g, &format!(r#"g.with("bulk.merge", {merge}).V("a").both().both().barrier().property(jpath("meta.k"), 5).toList()"#)).unwrap();
        let _ = run_on(&g, &format!(r#"g.with("bulk.merge", {merge}).V("a").property(jpath("log"), []).both().both().barrier().property(jpath("log[-1]"), 1).toList()"#)).unwrap_err();
        list(&g, r#"g.V().values(jpath("meta.k")).toList()"#)
    };
    assert_eq!(state(true), state(false));
    // Appending once per traverser: 3 traversers reach `a`'s neighbours, each appends once.
    for merge in [true, false] {
        let g = fresh();
        let _ = run_on(&g, r#"g.V("a").property(jpath("log"), []).toList()"#).unwrap();
        let _ = run_on(&g, &format!(r#"g.with("bulk.merge", {merge}).V("a").out().in().barrier().property(jpath("log[0]"), 1).toList()"#)).unwrap();
        assert_eq!(
            list(&g, r#"g.V("a").values(jpath("log[0]")).toList()"#),
            ["1"]
        );
    }
}

#[test]
fn write_in_a_failed_traversal_is_rolled_back() {
    // Implicit auto-commit: a failing traversal rolls back every write it made, jpath or plain.
    let g = fresh();
    let err = err_of(
        &g,
        r#"g.V().hasLabel("p").property(jpath("meta.tags.x"), 1).toList()"#,
    );
    assert!(err.contains("is an array"), "{err}");
    let _ = run_on(&g, r#"g.V("c").property("w", 1).toList()"#).unwrap();
    let err = err_of(
        &g,
        r#"g.V("c").property("first", 1).property(jpath("w.x"), 2).toList()"#,
    );
    assert!(err.contains("'$.w' is a number"), "{err}");
    assert!(
        list(&g, r#"g.V("c").values("first").toList()"#).is_empty(),
        "the earlier write of the failed traversal is rolled back"
    );
    assert_eq!(
        list(&g, r#"g.V("c").values("w").toList()"#),
        ["1"],
        "the failed write itself changed nothing"
    );
}

#[test]
fn write_profile_text_and_plain_property_is_unchanged() {
    let g = fresh();
    let profile = run_on(
        &g,
        r#"g.V("a").property(jpath("meta.k"), 2).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(
        profile.contains(r#"property(jpath("$.meta.k"), 2)"#),
        "{profile}"
    );
    let plain = run_on(&g, r#"g.V("a").property("meta", 2).profile().to_string()"#)
        .unwrap()
        .to_string();
    assert!(plain.contains(r#"property("meta", 2)"#), "{plain}");
}

#[test]
fn write_rust_api_accepts_names_and_paths() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v("p")
        .property("name", "n")
        .property(jp("meta.a.b"), 1)
        .property(
            jp("meta.l"),
            ElementProperty::Array(vec![ElementProperty::from(1)]),
        )
        .next()
        .unwrap();
    graph
        .traversal_mut()
        .v(None)
        .property(jp("meta.l[1]"), 2)
        .property(jp("meta.l[0]"), 0)
        .next()
        .unwrap();
    let got = graph
        .traversal()
        .v(None)
        .values(jp("meta.l[1]"))
        .next()
        .unwrap();
    assert_eq!(got, Some(ElementProperty::from(2)));
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .values(jp("meta.l[0]"))
            .next()
            .unwrap(),
        Some(ElementProperty::from(0))
    );
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .values(jp("meta.a.b"))
            .next()
            .unwrap(),
        Some(ElementProperty::from(1))
    );
    let err = graph
        .traversal_mut()
        .v(None)
        .property(jp("meta.l[5]"), 1)
        .next()
        .expect_err("out of range");
    assert!(
        err.root_cause().to_string().contains("out of range"),
        "{err}"
    );
    // cardinality + path
    let err = graph
        .traversal_mut()
        .v(None)
        .property_with(Cardinality::Single, jp("meta.a"), 1)
        .next()
        .expect_err("cardinality");
    assert!(
        err.root_cause().to_string().contains("not supported"),
        "{err}"
    );
    // anonymous traversal form
    graph
        .traversal_mut()
        .v(None)
        .property_by(jp("meta.copy"), __::values(jp("meta.a.b")))
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .values(jp("meta.copy"))
            .next()
            .unwrap(),
        Some(ElementProperty::from(1))
    );
}

// Task 9: removal, `remove_property(jpath(..))`.

fn rm(graph: &Arc<Graph>, script: &str) {
    let _ = run_on(graph, script).unwrap_or_else(|e| panic!("{script}: {e}"));
}

#[test]
fn remove_object_key_keeps_the_empty_parent() {
    let g = fresh();
    rm(&g, r#"g.V("a").remove_property(jpath("meta.k")).toList()"#);
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.k")).toList()"#),
        Vec::<String>::new()
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[0]")).toList()"#),
        ["x"]
    );
    // remove the only key of a nested object: the (now empty) parent stays
    rm(
        &g,
        r#"g.V("a").remove_property(jpath("meta.o.deep.n")).toList()"#,
    );
    let deep = run_on(&g, r#"g.V("a").values(jpath("meta.o.deep")).next()"#)
        .unwrap()
        .cast::<rhai::Map>();
    assert!(deep.is_empty());
    assert_eq!(
        list(&g, r#"g.V("a").has(jpath("meta.o.deep")).count().toList()"#),
        ["1"]
    );
}

#[test]
fn remove_array_element_shifts_and_negative_index_counts_from_the_end() {
    let g = fresh();
    rm(
        &g,
        r#"g.V("a").property(jpath("meta.tags[2]"), "z").property(jpath("meta.tags[3]"), "w").toList()"#,
    );
    // ["x","y","z","w"] -> remove [1] -> ["x","z","w"]
    rm(
        &g,
        r#"g.V("a").remove_property(jpath("meta.tags[1]")).toList()"#,
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[1]")).toList()"#),
        ["z"]
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[3]")).toList()"#),
        Vec::<String>::new()
    );
    // [-1] = last -> ["x","z"]
    rm(
        &g,
        r#"g.V("a").remove_property(jpath("meta.tags[-1]")).toList()"#,
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[-1]")).toList()"#),
        ["z"]
    );
    // [0] -> ["z"]
    rm(
        &g,
        r#"g.V("a").remove_property(jpath("meta.tags[0]")).toList()"#,
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[0]")).toList()"#),
        ["z"]
    );
    // an element object inside an array: remove below an index
    rm(
        &g,
        r#"g.V("a").property(jpath("meta.tags[1]"), #{q: 1, r: 2}).remove_property(jpath("meta.tags[1].q")).toList()"#,
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[1].q")).toList()"#),
        Vec::<String>::new()
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[1].r")).toList()"#),
        ["2"]
    );
}

#[test]
fn remove_missing_and_mismatching_paths_are_no_ops() {
    let g = fresh();
    let before = json_of(&g, "a", "meta");
    for path in [
        "meta.nope",        // missing key
        "meta.nope.deeper", // missing intermediate
        "meta.tags[9]",     // out of range
        "meta.tags[-9]",    // negative out of range
        "meta.tags.x",      // key step on an array
        "meta[0]",          // index step on an object
        "meta.k.x",         // descent through a scalar
        "meta.k[0]",        // index through a scalar
        "ghost",            // missing top-level property
        "ghost.x[0]",       // missing top-level property, deeper
        "[0]",              // first segment is an index
    ] {
        rm(
            &g,
            &format!(r#"g.V("a").remove_property(jpath("{path}")).toList()"#),
        );
        assert_eq!(json_of(&g, "a", "meta"), before, "{path}");
    }
    // an element without any properties, and an edge without the property
    rm(&g, r#"g.V("c").remove_property(jpath("meta.k")).toList()"#);
    rm(
        &g,
        r#"g.E("e1").remove_property(jpath("nothing.here")).toList()"#,
    );
    assert_eq!(list(&g, r#"g.V("c").valueMap().toList()"#), ["#{}"]);
}

#[test]
fn remove_single_segment_path_equals_plain_removal() {
    let g = fresh();
    rm(&g, r#"g.V("a").remove_property(jpath("meta")).toList()"#);
    assert_eq!(list(&g, r#"g.V("a").has("meta").count().toList()"#), ["0"]);
    // a literal dotted name is untouched by a path and removed by the plain string
    rm(
        &g,
        r#"g.V("c").property("x.y", 1).property_json(#{x: #{y: 2}}).toList()"#,
    );
    rm(&g, r#"g.V("c").remove_property(jpath("x.y")).toList()"#);
    assert_eq!(list(&g, r#"g.V("c").values("x.y").toList()"#), ["1"]);
    assert_eq!(
        list(&g, r#"g.V("c").values(jpath("x.y")).toList()"#),
        Vec::<String>::new()
    );
    rm(&g, r#"g.V("c").remove_property("x.y").toList()"#);
    assert_eq!(
        list(&g, r#"g.V("c").values("x.y").toList()"#),
        Vec::<String>::new()
    );
    // property(k, null) still stores a null (removal is explicit)
    rm(&g, r#"g.V("c").property("n", ()).toList()"#);
    assert_eq!(list(&g, r#"g.V("c").has("n").count().toList()"#), ["1"]);
    rm(&g, r#"g.V("c").remove_property(jpath("n")).toList()"#);
    assert_eq!(list(&g, r#"g.V("c").has("n").count().toList()"#), ["0"]);
}

#[test]
fn remove_mixes_names_paths_and_arrays_in_argument_order() {
    let g = fresh();
    rm(
        &g,
        r#"g.V("a").remove_property([jpath("meta.k"), "scalar", jpath("meta.tags[0]"), jpath("meta.tags[0]")]).toList()"#,
    );
    // both [0] removals applied in order: ["x","y"] -> ["y"] -> []
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.tags[0]")).toList()"#),
        Vec::<String>::new()
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.k")).toList()"#),
        Vec::<String>::new()
    );
    // the element continues in the stream
    assert_eq!(
        list(
            &g,
            r#"g.V("a").remove_property(jpath("meta.o.deep")).id().toList()"#
        ),
        ["a"]
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.o.deep.n")).toList()"#),
        Vec::<String>::new()
    );
    // an array with a wrong element type is still the plain error
    let err = err_of(
        &g,
        r#"g.V("a").remove_property([jpath("meta.k"), 5]).toList()"#,
    );
    assert!(err.contains("remove_property"), "{err}");
    assert!(err.contains("jpath(..)"), "{err}");
}

#[test]
fn remove_on_edges_and_with_many_traversers_and_bulk() {
    let g = fresh();
    rm(
        &g,
        r#"g.E("e1").remove_property(jpath("info.list[0]")).remove_property(jpath("info.rank")).toList()"#,
    );
    assert_eq!(
        list(&g, r#"g.E("e1").values(jpath("info.rank")).toList()"#),
        Vec::<String>::new()
    );
    assert_eq!(
        list(&g, r#"g.E("e1").values(jpath("info.list[0]")).toList()"#),
        Vec::<String>::new()
    );
    assert_eq!(
        list(&g, r#"g.E("e1").has(jpath("info.list")).count().toList()"#),
        ["1"]
    );
    // every vertex of a stream: bulk-n traversers (barrier merges) remove ONCE per distinct value
    // for the shifting removal only when merging is off; mutating plans never merge.
    for merge in ["true", "false"] {
        let g = fresh();
        rm(&g, r#"g.V("b").property("l", [1, 2, 3]).toList()"#);
        rm(
            &g,
            &format!(
                r#"g.with("bulk.merge", {merge}).V("a").out_e().in_v().barrier().remove_property(jpath("l[0]")).toList()"#
            ),
        );
        assert_eq!(
            list(&g, r#"g.V("b").values("l").toList()"#).len(),
            1,
            "merge={merge}"
        );
        assert_eq!(
            list(&g, r#"g.V("b").values(jpath("l[0]")).toList()"#),
            ["2"],
            "merge={merge}"
        );
    }
    // two traversers of two elements
    let g = fresh();
    rm(&g, r#"g.V("a", "b").property(jpath("m.l"), 1).toList()"#);
    rm(
        &g,
        r#"g.V("a", "b").remove_property(jpath("m.l")).toList()"#,
    );
    assert_eq!(
        list(&g, r#"g.V().has(jpath("m.l")).count().toList()"#),
        ["0"]
    );
}

#[test]
fn remove_interplays_with_path_writes_and_reads() {
    let g = fresh();
    rm(&g, r#"g.V("c").property(jpath("cfg.x"), 1).toList()"#);
    rm(&g, r#"g.V("c").property(jpath("cfg.l"), []).toList()"#);
    rm(
        &g,
        r#"g.V("c").property(jpath("cfg.l[0]"), "a").property(jpath("cfg.l[1]"), "b").toList()"#,
    );
    rm(
        &g,
        r#"g.V("c").remove_property(jpath("cfg.l[0]")).property(jpath("cfg.l[1]"), "c").toList()"#,
    );
    assert_eq!(
        list(&g, r#"g.V("c").values(jpath("cfg.l[0]")).toList()"#),
        ["b"]
    );
    assert_eq!(
        list(&g, r#"g.V("c").values(jpath("cfg.l[1]")).toList()"#),
        ["c"]
    );
    // remove then re-create the key
    rm(&g, r#"g.V("c").remove_property(jpath("cfg.x")).toList()"#);
    assert_eq!(
        list(&g, r#"g.V().has(jpath("cfg.x")).count().toList()"#),
        ["0"]
    );
    rm(&g, r#"g.V("c").property(jpath("cfg.x"), 2).toList()"#);
    assert_eq!(
        list(&g, r#"g.V().has(jpath("cfg.x"), 2).count().toList()"#),
        ["1"]
    );
}

#[test]
fn remove_in_an_anonymous_traversal_and_profile_text() {
    let g = fresh();
    rm(
        &g,
        r#"g.V("a").local(__.remove_property(jpath("meta.k"))).toList()"#,
    );
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.k")).toList()"#),
        Vec::<String>::new()
    );
    let profile = run_on(
        &g,
        r#"g.V("a").remove_property([jpath("meta.k"), "scalar"]).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(
        profile.contains(r#"remove_property([jpath("$.meta.k"), "scalar"])"#),
        "{profile}"
    );
    let plain = run_on(
        &g,
        r#"g.V("a").remove_property("age").profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(plain.contains(r#"remove_property("age")"#), "{plain}");
}

#[test]
fn remove_rust_api_accepts_names_and_paths() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v("p")
        .property("name", "n")
        .property("age", 3)
        .property(jp("meta.a.b"), 1)
        .property(
            jp("meta.l"),
            ElementProperty::Array(vec![ElementProperty::from(1), ElementProperty::from(2)]),
        )
        .next()
        .unwrap();
    graph
        .traversal_mut()
        .v(None)
        .remove_property("age")
        .next()
        .unwrap();
    graph
        .traversal_mut()
        .v(None)
        .remove_property(vec!["name"])
        .next()
        .unwrap();
    assert_eq!(
        graph.traversal().v(None).values("age").next().unwrap(),
        None
    );
    graph
        .traversal_mut()
        .v(None)
        .remove_property(jp("meta.l[0]"))
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .values(jp("meta.l[0]"))
            .next()
            .unwrap(),
        Some(ElementProperty::from(2))
    );
    graph
        .traversal_mut()
        .v(None)
        .remove_property(vec![jp("meta.a.b"), jp("meta.nothing")])
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .values(jp("meta.a.b"))
            .next()
            .unwrap(),
        None
    );
    // anonymous form + no argument removes everything
    graph
        .traversal_mut()
        .v(None)
        .local(__::remove_property(jp("meta.l[0]")))
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .values(jp("meta.l[0]"))
            .next()
            .unwrap(),
        None
    );
    graph
        .traversal_mut()
        .v(None)
        .remove_property(None::<()>)
        .next()
        .unwrap();
    assert_eq!(
        graph.traversal().v(None).values("meta").next().unwrap(),
        None
    );
}

#[test]
fn remove_through_the_schema_layer_validates_the_whole_property() {
    use graphersal::prelude::*;
    let schema = GraphSchema::new().with_mode(SchemaMode::Open).with_vertex(
        "T",
        PropertySchema::object().with_required_property(
            "tags",
            PropertySchema::array(PropertySchema::string())
                .with_constraint(|c| c.min_items = Some(2)),
        ),
    );
    let mut graph = TraversalGraph::new();
    graph.set_schema(schema).unwrap();
    let mut p = PropertyMap::default();
    p.insert(
        "tags",
        ElementProperty::Array(vec!["a".into(), "b".into(), "c".into()]),
    );
    graph
        .add_vertex(Some("1".into()), vec!["T".into()], p)
        .unwrap();
    // 3 -> 2 elements: still valid
    graph
        .traversal_mut()
        .v("1")
        .remove_property(jp("tags[0]"))
        .next()
        .unwrap();
    // 2 -> 1 violates min_items: refused, the property is as it was
    let err = graph
        .traversal_mut()
        .v("1")
        .remove_property(jp("tags[0]"))
        .next()
        .expect_err("min_items");
    assert!(
        err.root_cause()
            .to_string()
            .to_lowercase()
            .contains("items"),
        "{err}"
    );
    assert_eq!(
        graph
            .traversal()
            .v("1")
            .values(jp("tags[1]"))
            .next()
            .unwrap(),
        Some(ElementProperty::from("c"))
    );
    // a missing path never reaches the schema layer
    graph
        .traversal_mut()
        .v("1")
        .remove_property(jp("tags[7]"))
        .next()
        .unwrap();
    // removing the whole (required) property is rejected
    let err = graph
        .traversal_mut()
        .v("1")
        .remove_property(jp("tags"))
        .next()
        .expect_err("required");
    assert!(
        err.root_cause()
            .to_string()
            .contains("Cannot remove required"),
        "{err}"
    );
    assert!(
        graph
            .traversal()
            .v("1")
            .values("tags")
            .next()
            .unwrap()
            .is_some()
    );
}

#[test]
fn properties_with_a_path_drop_stays_an_error_pointing_at_remove_property() {
    let g = fresh();
    let err = err_of(
        &g,
        r#"g.V("a").properties(jpath("meta.k")).drop().toList()"#,
    );
    assert!(err.contains("pathProperty"), "{err}");
    assert!(err.contains("remove_property(jpath("), "{err}");
    // nothing was removed
    assert_eq!(
        list(&g, r#"g.V("a").values(jpath("meta.k")).toList()"#),
        ["1"]
    );
}

// ---------------------------------------------------------------------------------------------
// Task 10: schema modes with nested paths.
// ---------------------------------------------------------------------------------------------

mod nested_schema {
    use super::*;

    const UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    /// A key: `nullable` is the old "optional" (absent or null allowed), otherwise required.
    type Field = (PropertySchema, bool);

    fn field(ty: PropertySchema, nullable: bool) -> Field {
        if nullable {
            (ty.nullable(), false)
        } else {
            (ty, true)
        }
    }

    fn with(f: Field, build: impl FnOnce(&mut Constraints)) -> Field {
        (f.0.with_constraint(build), f.1)
    }

    fn object(fields: Vec<(&str, Field)>) -> PropertySchema {
        crate::schema_support::object(
            fields
                .into_iter()
                .map(|(k, (node, required))| (k, node, required))
                .collect(),
        )
    }

    /// `P.meta` (nullable at the top level):
    /// `{k: int64 required, w: float64?, id: uuid?, name: string? min_length 2,
    ///   tags: array<string>? min_items 1, o: {n: int64 required}?, u: union<int64|string>?}`;
    /// `P.items`: `array<{n: int64 required}>?`; `E.info`: `{rank: int64 required, w: float64?}?`.
    fn schema(mode: SchemaMode) -> GraphSchema {
        let meta = object(vec![
            ("k", field(PropertySchema::integer(), false)),
            ("w", field(PropertySchema::number(), true)),
            ("id", field(PropertySchema::uuid(), true)),
            (
                "name",
                with(field(PropertySchema::string(), true), |c| {
                    c.min_length = Some(2)
                }),
            ),
            (
                "tags",
                with(
                    field(PropertySchema::array(PropertySchema::string()), true),
                    |c| c.min_items = Some(1),
                ),
            ),
            (
                "o",
                field(
                    object(vec![("n", field(PropertySchema::integer(), false))]),
                    true,
                ),
            ),
            (
                "u",
                field(
                    PropertySchema::any_of(vec![
                        PropertySchema::integer(),
                        PropertySchema::string(),
                    ]),
                    true,
                ),
            ),
        ]);
        let props = object(vec![
            ("meta", field(meta, true)),
            (
                "items",
                field(
                    PropertySchema::array(object(vec![(
                        "n",
                        field(PropertySchema::integer(), false),
                    )])),
                    true,
                ),
            ),
        ]);
        let eprops = object(vec![(
            "info",
            field(
                object(vec![
                    ("rank", field(PropertySchema::integer(), false)),
                    ("w", field(PropertySchema::number(), true)),
                ]),
                true,
            ),
        )]);
        GraphSchema::new()
            .with_mode(mode)
            .with_vertex("P", props)
            .with_edge("E", EdgeSchema::new(eprops).with_connection("P", "P"))
    }

    /// Data is inserted without a schema, then the schema is switched on.
    fn graph(mode: SchemaMode) -> Arc<Graph> {
        let g = Arc::new(GraphSource::empty());
        let _ = run_on(
            &g,
            r#"
            g.addV("P").property(T.id, "a").property_json(#{meta: #{k: 1, tags: ["x"], o: #{n: 1}}, items: [#{n: 1}, #{n: 2}]}).toList();
            g.addV("P").property(T.id, "b").toList();
            g.V("a").addE("E").to(__.V("b")).property(T.id, "e1").property("info", #{rank: 3}).toList();
            1
            "#,
        )
        .expect("fixture");
        g.write().set_schema(schema(mode)).unwrap();
        g
    }

    const MODES: [(&str, SchemaMode); 3] = [
        ("none", SchemaMode::None),
        ("open", SchemaMode::Open),
        ("closed", SchemaMode::Closed),
    ];

    fn show(g: &Arc<Graph>, script: &str) -> String {
        run_on(g, script)
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .to_string()
    }

    /// Expectation per mode for a write/removal: `Ok`, or an error containing the text.
    #[derive(Clone, Copy)]
    enum Want {
        Ok,
        Err(&'static str),
    }

    fn expect(mode: &str, g: &Arc<Graph>, script: &str, want: Want) {
        let result = run_on(g, script);
        match (want, result) {
            (Want::Ok, Ok(_)) => {}
            (Want::Ok, Err(e)) => panic!("[{mode}] {script} must succeed: {e}"),
            (Want::Err(text), Err(e)) => assert!(
                e.contains(text),
                "[{mode}] {script}: expected '{text}' in: {e}"
            ),
            (Want::Err(text), Ok(v)) => {
                panic!("[{mode}] {script} must fail with '{text}', got {v}")
            }
        }
    }

    /// One matrix row: (script, none, open, closed).
    fn row(script: &str, none: Want, open: Want, closed: Want) {
        for (mode, want) in [("none", none), ("open", open), ("closed", closed)] {
            let m = MODES.iter().find(|(n, _)| *n == mode).unwrap().1;
            expect(mode, &graph(m), script, want);
        }
    }

    use Want::{Err as E, Ok as O};

    #[test]
    fn leaf_write_matrix_on_vertices() {
        // leaf ok
        row(r#"g.V("a").property(jpath("meta.k"), 5).toList()"#, O, O, O);
        // wrong type: only enforced with a schema, the error names the full canonical path
        row(
            r#"g.V("a").property(jpath("meta.k"), "x").toList()"#,
            O,
            E("$.meta.k"),
            E("$.meta.k"),
        );
        row(
            r#"g.V("a").property(jpath("meta.o.n"), "x").toList()"#,
            O,
            E("$.meta.o.n"),
            E("$.meta.o.n"),
        );
        // undeclared nested key: allowed in none/open, rejected in closed
        row(
            r#"g.V("a").property(jpath("meta.extra"), 1).toList()"#,
            O,
            O,
            E("$.meta.extra"),
        );
        row(
            r#"g.V("a").property(jpath("meta.o.deeper.x"), 1).toList()"#,
            O,
            O,
            E("$.meta.o.deeper"),
        );
        // array: append at [len], replace at [0], wrong item type, undeclared item key
        row(
            r#"g.V("a").property(jpath("meta.tags[1]"), "y").toList()"#,
            O,
            O,
            O,
        );
        row(
            r#"g.V("a").property(jpath("meta.tags[0]"), "z").toList()"#,
            O,
            O,
            O,
        );
        row(
            r#"g.V("a").property(jpath("meta.tags[0]"), 5).toList()"#,
            O,
            E("$.meta.tags[0]"),
            E("$.meta.tags[0]"),
        );
        row(
            r#"g.V("a").property(jpath("items[0].n"), 9).toList()"#,
            O,
            O,
            O,
        );
        row(
            r#"g.V("a").property(jpath("items[1].n"), "s").toList()"#,
            O,
            E("$.items[1].n"),
            E("$.items[1].n"),
        );
        row(
            r#"g.V("a").property(jpath("items[0].x"), 1).toList()"#,
            O,
            O,
            E("$.items[0].x"),
        );
        // constraints at a nested location
        row(
            r#"g.V("a").property(jpath("meta.name"), "a").toList()"#,
            O,
            E("$.meta.name"),
            E("$.meta.name"),
        );
        row(
            r#"g.V("a").property(jpath("meta.name"), "ab").toList()"#,
            O,
            O,
            O,
        );
        // a path creating the property: the object it creates must satisfy the declaration
        row(r#"g.V("b").property(jpath("meta.k"), 1).toList()"#, O, O, O);
        row(
            r#"g.V("b").property(jpath("meta.w"), 1.5).toList()"#,
            O,
            E("$.meta.k"),
            E("$.meta.k"),
        );
        // a mismatch of the structure stays the (schema-independent) path error
        row(
            r#"g.V("a").property(jpath("meta.k.z"), 1).toList()"#,
            E("meta.k"),
            E("meta.k"),
            E("meta.k"),
        );
    }

    #[test]
    fn coercion_at_the_declared_nested_type() {
        for (mode, m) in MODES {
            let g = graph(m);
            let _ = run_on(&g, r#"g.V("a").property(jpath("meta.w"), 3).property(jpath("meta.id"), "550e8400-e29b-41d4-a716-446655440000").toList()"#)
                .unwrap_or_else(|e| panic!("[{mode}] {e}"));
            let w = show(&g, r#"g.V("a").values(jpath("meta.w")).next()"#);
            let is_uuid = show(
                &g,
                &format!(
                    r#"g.V("a").has(jpath("meta.id"), UUID.fromString("{UUID}")).count().next()"#
                ),
            ) == "1";
            if mode == "none" {
                assert_eq!(w, "3", "[{mode}] no coercion without a schema");
                assert!(!is_uuid, "[{mode}] stays a string");
            } else {
                assert_eq!(w, "3.0", "[{mode}] int64 -> float64 at the nested leaf");
                assert!(is_uuid, "[{mode}] canonical string -> uuid");
            }
        }
        // a non-canonical uuid string does not coerce and names the path
        for (mode, m) in &MODES[1..] {
            let err = run_on(
                &graph(*m),
                r#"g.V("a").property(jpath("meta.id"), "nope").toList()"#,
            )
            .expect_err("coercion");
            assert!(
                err.contains("$.meta.id") && err.contains("uuid"),
                "[{mode}] {err}"
            );
        }
        // array items and an inexact int64 -> float64
        let g = graph(SchemaMode::Open);
        let err = run_on(
            &g,
            r#"g.V("a").property(jpath("meta.w"), 9007199254740993).toList()"#,
        )
        .expect_err("inexact");
        assert!(err.contains("$.meta.w"), "{err}");
        // reads never coerce: the stored string stays a string
        let g = graph(SchemaMode::None);
        let _ = run_on(
            &g,
            &format!(r#"g.V("a").property(jpath("meta.id"), "{UUID}").toList()"#),
        )
        .unwrap();
        // Applying a schema never coerces stored data: the string in a `uuid` field is a violation
        // that fails the application (decision C), and the graph keeps the string.
        let err = g
            .write()
            .set_schema(schema(SchemaMode::Open))
            .expect_err("string in a uuid field");
        assert!(err.to_string().contains("$.meta.id"), "{err}");
        assert_eq!(
            show(
                &g,
                &format!(
                    r#"g.V("a").has(jpath("meta.id"), UUID.fromString("{UUID}")).count().next()"#
                )
            ),
            "0"
        );
    }

    #[test]
    fn union_leaf() {
        row(r#"g.V("a").property(jpath("meta.u"), 5).toList()"#, O, O, O);
        row(
            r#"g.V("a").property(jpath("meta.u"), "s").toList()"#,
            O,
            O,
            O,
        );
        row(
            r#"g.V("a").property(jpath("meta.u"), true).toList()"#,
            O,
            E("$.meta.u"),
            E("$.meta.u"),
        );
    }

    #[test]
    fn union_ambiguity_prefers_the_exact_variant_else_errors() {
        // p: union<object{a: int64} | object{a: string}>
        let ty = PropertySchema::any_of(vec![
            object(vec![("a", field(PropertySchema::integer(), false))]),
            object(vec![("a", field(PropertySchema::string(), false))]),
        ]);
        let s = GraphSchema::new()
            .with_mode(SchemaMode::Open)
            .with_vertex("U", object(vec![("p", field(ty, true))]));
        let g = Arc::new(GraphSource::empty());
        let _ = run_on(
            &g,
            r#"g.addV("U").property(T.id, "u").property_json(#{p: #{a: 1}}).toList(); 1"#,
        )
        .unwrap();
        g.write().set_schema(s).unwrap();
        let _ = run_on(&g, r#"g.V("u").property(jpath("p.a"), 5).toList()"#)
            .expect("int64 variant is exact");
        let _ = run_on(&g, r#"g.V("u").property(jpath("p.a"), "s").toList()"#)
            .expect("string variant is exact");
        let err = run_on(&g, r#"g.V("u").property(jpath("p.a"), 1.5).toList()"#)
            .expect_err("no exact variant");
        assert!(
            err.contains("$.p.a") && err.contains("union<int64|string>"),
            "{err}"
        );
    }

    #[test]
    fn whole_object_writes_validate_nested_content() {
        // plain property(name, object) and property_json go through the same nested validation
        for script in [
            r#"g.V("a").property("meta", #{k: "bad"}).toList()"#,
            r#"g.V("a").property_json(#{meta: #{k: "bad"}}).toList()"#,
        ] {
            row(script, O, E("$.meta.k"), E("$.meta.k"));
        }
        // a missing required nested key
        row(
            r#"g.V("a").property("meta", #{w: 1.5}).toList()"#,
            O,
            E("$.meta.k"),
            E("$.meta.k"),
        );
        // an undeclared nested key: only closed rejects it
        row(
            r#"g.V("a").property("meta", #{k: 1, zzz: 1}).toList()"#,
            O,
            O,
            E("$.meta.zzz"),
        );
        // an explicit null in a nullable nested field is fine, in a required one it is not
        row(
            r#"g.V("a").property("meta", #{k: 1, w: ()}).toList()"#,
            O,
            O,
            O,
        );
        row(
            r#"g.V("a").property("meta", #{k: ()}).toList()"#,
            O,
            E("$.meta.k"),
            E("$.meta.k"),
        );
    }

    #[test]
    fn removal_matrix_is_refused_when_the_schema_would_be_violated() {
        // a required nested key
        row(
            r#"g.V("a").remove_property(jpath("meta.k")).toList()"#,
            O,
            E("$.meta.k"),
            E("$.meta.k"),
        );
        // min_items (tags has exactly one element)
        row(
            r#"g.V("a").remove_property(jpath("meta.tags[0]")).toList()"#,
            O,
            E("$.meta.tags"),
            E("$.meta.tags"),
        );
        // an optional nested key and a nested object
        row(
            r#"g.V("a").remove_property(jpath("meta.o")).toList()"#,
            O,
            O,
            O,
        );
        row(
            r#"g.V("a").remove_property(jpath("meta.o.n")).toList()"#,
            O,
            E("$.meta.o.n"),
            E("$.meta.o.n"),
        );
        // array element of a declared array of objects
        row(
            r#"g.V("a").remove_property(jpath("items[0]")).toList()"#,
            O,
            O,
            O,
        );
        // no-op removals never error
        row(
            r#"g.V("a").remove_property(jpath("meta.nope")).remove_property(jpath("meta.tags[9]")).remove_property(jpath("zzz.y")).toList()"#,
            O,
            O,
            O,
        );
        // a refused removal leaves the property untouched
        let g = graph(SchemaMode::Open);
        run_on(&g, r#"g.V("a").remove_property(jpath("meta.k")).toList()"#).expect_err("refused");
        assert_eq!(show(&g, r#"g.V("a").values(jpath("meta.k")).next()"#), "1");
    }

    #[test]
    fn edges_follow_the_same_rules() {
        row(
            r#"g.E("e1").property(jpath("info.rank"), 4).toList()"#,
            O,
            O,
            O,
        );
        row(
            r#"g.E("e1").property(jpath("info.rank"), "x").toList()"#,
            O,
            E("$.info.rank"),
            E("$.info.rank"),
        );
        row(
            r#"g.E("e1").property(jpath("info.extra"), 1).toList()"#,
            O,
            O,
            E("$.info.extra"),
        );
        row(
            r#"g.E("e1").remove_property(jpath("info.rank")).toList()"#,
            O,
            E("$.info.rank"),
            E("$.info.rank"),
        );
        for (mode, m) in MODES {
            let g = graph(m);
            let _ = run_on(&g, r#"g.E("e1").property(jpath("info.w"), 2).toList()"#).unwrap();
            let w = show(&g, r#"g.E("e1").values(jpath("info.w")).next()"#);
            assert_eq!(w, if mode == "none" { "2" } else { "2.0" }, "[{mode}]");
        }
    }

    #[test]
    fn rust_api_notation() {
        for (mode, m) in MODES {
            let g = graph(m);
            let mut lock = g.write();
            let ok = lock
                .traversal_mut()
                .v("a")
                .property(jp("meta.w"), 3_i64)
                .next();
            assert!(ok.is_ok(), "[{mode}] {ok:?}");
            let bad = lock
                .traversal_mut()
                .v("a")
                .property(jp("meta.k"), "x")
                .next();
            assert_eq!(bad.is_err(), mode != "none", "[{mode}]");
            if let Err(e) = bad {
                assert!(e.root_cause().to_string().contains("$.meta.k"), "{e}");
            }
            let rm = lock
                .traversal_mut()
                .v("a")
                .remove_property(jp("meta.k"))
                .next();
            assert_eq!(rm.is_err(), mode != "none", "[{mode}]");
        }
    }

    #[test]
    fn without_a_schema_everything_is_as_before() {
        // SchemaMode::None with declarations present and a graph that never had a schema agree.
        let plain = Arc::new(GraphSource::empty());
        let _ = run_on(
            &plain,
            r#"g.addV("P").property(T.id, "a").property_json(#{meta: #{k: 1, tags: ["x"], o: #{n: 1}}}).toList(); 1"#,
        )
        .unwrap();
        let declared = graph(SchemaMode::None);
        for script in [
            r#"g.V("a").property(jpath("meta.k"), "x").property(jpath("meta.w"), 3).property(jpath("meta.extra.deep"), 1).property(jpath("meta.tags[1]"), 7).toList()"#,
            r#"g.V("a").remove_property(jpath("meta.k")).remove_property(jpath("meta.tags[0]")).toList()"#,
        ] {
            let _ = run_on(&plain, script).unwrap();
            let _ = run_on(&declared, script).unwrap();
        }
        let q = r#"g.V("a").valueMap("meta").toList()"#;
        assert_eq!(show(&plain, q), show(&declared, q));
    }

    #[test]
    fn inferred_and_evolved_schemas_accept_nested_path_writes() {
        // inference: nested object/array fields are described and then enforced
        let g = Arc::new(GraphSource::empty());
        let _ = run_on(
            &g,
            r#"g.addV("P").property(T.id, "a").property_json(#{meta: #{k: 1, w: 1.5, tags: ["x"]}}).toList();
               g.addV("P").property(T.id, "b").property_json(#{meta: #{k: 2, w: 2.5, tags: ["y"]}}).toList(); 1"#,
        )
        .unwrap();
        let inferred = g
            .read()
            .traversal()
            .infer_schema()
            .unwrap()
            .with_mode(SchemaMode::Open);
        let meta = &inferred.vertices["P"].schema.properties["meta"];
        assert!(meta.properties.contains_key("k") && meta.properties.contains_key("tags"));
        g.write().set_schema(inferred.clone()).unwrap();
        let _ = run_on(&g, r#"g.V("a").property(jpath("meta.k"), 7).property(jpath("meta.w"), 2).property(jpath("meta.tags[1]"), "z").toList()"#)
            .expect("conforming nested writes");
        assert_eq!(
            show(&g, r#"g.V("a").values(jpath("meta.w")).next()"#),
            "2.0",
            "int64 -> float64 via the inferred type"
        );
        let err =
            run_on(&g, r#"g.V("a").property(jpath("meta.k"), "x").toList()"#).expect_err("type");
        assert!(err.contains("$.meta.k"), "{err}");
        // the inferred schema shows nested types in the visualizers
        let table = inferred.to_table().to_string();
        assert!(table.contains("object"), "{table}");

        // evolution: a required nested key added through a merge patch is enforced on the next
        // write
        let evolved = SchemaPatch::from_json(
            r#"{"vertices": {"P": {"schema": {"properties": {"meta": {
                "properties": {"label": {"type": "string"}},
                "required": ["k", "w", "tags", "label"]}}}}}}"#,
        )
        .unwrap()
        .apply(Some(&inferred))
        .expect("patch");
        // `label` is required now and missing from the stored objects: applying the evolved schema
        // to this data fails and names the nested path (decision C) ...
        let err = g
            .write()
            .set_schema(evolved.clone())
            .expect_err("required nested key missing");
        assert!(err.to_string().contains("$.meta.label"), "{err}");
        // ... so the data is migrated first (the current Open schema allows the undeclared key).
        let _ = run_on(
            &g,
            r#"g.V("b").property(jpath("meta.label"), "B").toList()"#,
        )
        .unwrap();
        let _ = run_on(
            &g,
            r#"g.V("a").property(jpath("meta.label"), "A").toList()"#,
        )
        .unwrap();
        g.write().set_schema(evolved).unwrap();
        // the nested required key cannot be removed again
        let err = run_on(
            &g,
            r#"g.V("a").remove_property(jpath("meta.label")).toList()"#,
        )
        .expect_err("required");
        assert!(err.contains("$.meta.label"), "{err}");
        let _ = run_on(&g, r#"g.V("a").property(jpath("meta.k"), 8).toList()"#)
            .expect("valid again after the migration");
    }
}

// Task 11: DSL parity matrix. Every step that takes a property name is exercised with a jpath in
// both spellings (snake_case and Gremlin camelCase) and as an explicit-`__.` anonymous step. A
// pair must give the same result; the first element of a pair is the snake_case spelling.
#[test]
fn dsl_parity_every_name_taking_step_in_both_spellings() {
    let g = nested();
    let pairs: &[(&str, &str)] = &[
        (
            r#"g.v().values(jpath("meta.k")).to_list()"#,
            r#"g.V().values(jpath("meta.k")).toList()"#,
        ),
        (
            r#"g.v().properties(jpath("meta.k")).value().to_list()"#,
            r#"g.V().properties(jpath("meta.k")).value().toList()"#,
        ),
        (
            r#"g.v().has(jpath("meta.k"), 3).count().to_list()"#,
            r#"g.V().has(jpath("meta.k"), 3).count().toList()"#,
        ),
        (
            r#"g.v().has(jpath("meta.k"), P.gt(0)).count().to_list()"#,
            r#"g.V().has(jpath("meta.k"), P.gt(0)).count().toList()"#,
        ),
        (
            r#"g.v().has(jpath("meta.k")).count().to_list()"#,
            r#"g.V().has(jpath("meta.k")).count().toList()"#,
        ),
        (
            r#"g.v().has_not(jpath("meta.k")).count().to_list()"#,
            r#"g.V().hasNot(jpath("meta.k")).count().toList()"#,
        ),
        (
            r#"g.v().has("person", jpath("meta.k"), P.gt(0)).count().to_list()"#,
            r#"g.V().has("person", jpath("meta.k"), P.gt(0)).count().toList()"#,
        ),
        (
            r#"g.v().value_map(jpath("meta.k")).to_list()"#,
            r#"g.V().valueMap(jpath("meta.k")).toList()"#,
        ),
        (
            r#"g.v().element_map(jpath("meta.k")).to_list()"#,
            r#"g.V().elementMap(jpath("meta.k")).toList()"#,
        ),
        (
            r#"g.v().order().by(jpath("meta.k")).values("name").to_list()"#,
            r#"g.V().order().by(jpath("meta.k")).values("name").toList()"#,
        ),
        (
            r#"g.v().group_count().by(jpath("meta.k")).to_list()"#,
            r#"g.V().groupCount().by(jpath("meta.k")).toList()"#,
        ),
        (
            r#"g.v().where(__.values(jpath("meta.k"))).count().to_list()"#,
            r#"g.V().where(__.values(jpath("meta.k"))).count().toList()"#,
        ),
        (
            r#"g.v().filter(__.has(jpath("meta.k"))).count().to_list()"#,
            r#"g.V().filter(__.has(jpath("meta.k"))).count().toList()"#,
        ),
        (
            r#"g.v().local(__.values(jpath("meta.k"))).to_list()"#,
            r#"g.V().local(__.values(jpath("meta.k"))).toList()"#,
        ),
        (
            r#"g.v().local(__.has_not(jpath("meta.k"))).count().to_list()"#,
            r#"g.V().local(__.hasNot(jpath("meta.k"))).count().toList()"#,
        ),
        (
            r#"g.v().local(__.value_map(jpath("meta.k"))).to_list()"#,
            r#"g.V().local(__.valueMap(jpath("meta.k"))).toList()"#,
        ),
        (
            r#"g.v().local(__.element_map(jpath("meta.k"))).to_list()"#,
            r#"g.V().local(__.elementMap(jpath("meta.k"))).toList()"#,
        ),
    ];
    for (snake, camel) in pairs {
        let a = run_on(&g, snake);
        let b = run_on(&g, camel);
        match (a, b) {
            (Ok(a), Ok(b)) => assert_eq!(a.to_string(), b.to_string(), "{snake} vs {camel}"),
            (a, b) => panic!("{snake} / {camel}: {a:?} / {b:?}"),
        }
    }
    // Write and remove spellings (`property` has no camelCase twin in Gremlin).
    for script in [
        r#"g.V("1").property(jpath("meta.w"), 1).values(jpath("meta.w")).toList()"#,
        r#"g.V("1").property(jpath("meta.w"), 1).values(jpath("meta.w")).to_list()"#,
    ] {
        assert_eq!(list(&g, script), ["1"], "{script}");
    }
    for script in [
        r#"g.V("1").remove_property(jpath("meta.k")).values(jpath("meta.k")).toList()"#,
        r#"g.V("1").remove_property(jpath("meta.k")).values(jpath("meta.k")).toList()"#,
    ] {
        assert!(list(&g, script).is_empty(), "{script}");
    }
}

#[test]
fn rust_anonymous_map_steps_accept_path_keys() {
    let g = nested();
    let lock = g.read();
    let t = lock.traversal();
    let n = t
        .v(None::<()>)
        .local(__::value_map(jp("meta.k")))
        .to_list()
        .expect("value_map")
        .len();
    assert_eq!(n, 6);
    let t = lock.traversal();
    assert_eq!(
        t.v(None::<()>)
            .local(__::element_map(jp("meta.k")))
            .to_list()
            .expect("element_map")
            .len(),
        6
    );
    let t = lock.traversal();
    assert_eq!(
        t.v(None::<()>)
            .local(__::value_map_with_tokens(jp("meta.k")))
            .to_list()
            .expect("tokens")
            .len(),
        6
    );
}
