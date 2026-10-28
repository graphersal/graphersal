//! What a `by()` projection reads from each stream shape (`TraverserValue::fetch_*`): a property
//! name, `T.id`/`T.label`, a `jpath(..)` and a `Column` over object-valued vertex and edge
//! properties, materialized maps (`elementMap()`, `valueMap()`, `project()`, an injected map),
//! lists and paths. A shape without the key is an unproductive `by()` (the `project()` key is
//! left out), never an error.
#![cfg(feature = "script")]

use std::sync::Arc;

use crate::script_host::eval_value;
use graphersal::prelude::*;

/// d1 {a: 100, meta: {a: 1, b: "x"}}, d2 {meta: {b: "y"}}, edge d2 -ref-> d1 {info: {w: 2},
/// note: "n"}.
fn docs() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::empty());
    let _ = eval_value(
        graph.clone(),
        r#"
        g.addV("doc").property("name", "d1").property("a", 100)
            .property("meta", #{a: 1, b: "x"}).next();
        g.addV("doc").property("name", "d2").property("meta", #{b: "y"}).next();
        g.V().has("name", "d2").addE("ref").to(__.V().has("name", "d1"))
            .property("info", #{w: 2}).property("note", "n").next();
        "#,
    )
    .expect("fixture");
    graph
}

/// Runs `script` in both notations' results as rendered text, one entry per result.
fn rendered(graph: &Arc<Graph>, script: &str) -> Vec<String> {
    eval_value(graph.clone(), script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
        .into_array()
        .unwrap_or_else(|t| panic!("{script}: not a list but {t}"))
        .into_iter()
        .map(|v| v.to_string())
        .collect()
}

fn same(graph: &Arc<Graph>, camel: &str, snake: &str, expected: &[&str]) {
    let a = rendered(graph, camel);
    assert_eq!(a, expected, "{camel}");
    assert_eq!(rendered(graph, snake), a, "{snake} differs from {camel}");
}

#[test]
fn a_property_name_reads_inside_object_valued_properties() {
    let g = docs();
    same(
        &g,
        r#"g.V().properties("meta").project("x").by("b").toList()"#,
        r#"g.v().properties("meta").project("x").by("b").to_list()"#,
        &[r#"#{"x": "x"}"#, r#"#{"x": "y"}"#],
    );
    same(
        &g,
        r#"g.E().properties("info").project("x").by("w").toList()"#,
        r#"g.e().properties("info").project("x").by("w").to_list()"#,
        &[r#"#{"x": 2}"#],
    );
    // a scalar property has no keys: the by() is unproductive, not an error
    same(
        &g,
        r#"g.E().properties("note").project("x").by("w").toList()"#,
        r#"g.e().properties("note").project("x").by("w").to_list()"#,
        &["#{}"],
    );
    // a path has no named property either
    same(
        &g,
        r#"g.V().path().project("x").by("name").toList()"#,
        r#"g.v().path().project("x").by("name").to_list()"#,
        &["#{}", "#{}"],
    );
}

#[test]
fn a_jpath_walks_properties_maps_and_lists() {
    let g = docs();
    same(
        &g,
        r#"g.V().properties("meta").project("j").by(jpath("b")).toList()"#,
        r#"g.v().properties("meta").project("j").by(jpath("b")).to_list()"#,
        &[r#"#{"j": "x"}"#, r#"#{"j": "y"}"#],
    );
    same(
        &g,
        r#"g.E().properties("info").project("j").by(jpath("w")).toList()"#,
        r#"g.e().properties("info").project("j").by(jpath("w")).to_list()"#,
        &[r#"#{"j": 2}"#],
    );
    // an edge reads the first segment as a stored key
    same(
        &g,
        r#"g.E().project("j").by(jpath("info.w")).toList()"#,
        r#"g.e().project("j").by(jpath("info.w")).to_list()"#,
        &[r#"#{"j": 2}"#],
    );
    // a record (valueMap) and an object (elementMap through project) take the first key
    same(
        &g,
        r#"g.V().valueMap().project("j").by(jpath("name[0]")).toList()"#,
        r#"g.v().value_map().project("j").by(jpath("name[0]")).to_list()"#,
        &[r#"#{"j": "d1"}"#, r#"#{"j": "d2"}"#],
    );
    same(
        &g,
        r#"g.V().project("p").by(__.elementMap()).select("p").project("j").by(jpath("meta.b")).toList()"#,
        r#"g.v().project("p").by(__.element_map()).select("p").project("j").by(jpath("meta.b")).to_list()"#,
        &[r#"#{"j": "x"}"#, r#"#{"j": "y"}"#],
    );
    // a list takes an index; a key on a list or an index on a map finds nothing
    same(
        &g,
        r#"g.V().values("name").fold().project("j", "k").by(jpath("[-1]")).by(jpath("a")).toList()"#,
        r#"g.v().values("name").fold().project("j", "k").by(jpath("[-1]")).by(jpath("a")).to_list()"#,
        &[r#"#{"j": "d2"}"#],
    );
    same(
        &g,
        r#"g.V().valueMap().project("j").by(jpath("[0]")).toList()"#,
        r#"g.v().value_map().project("j").by(jpath("[0]")).to_list()"#,
        &["#{}", "#{}"],
    );
}

#[test]
fn tokens_read_ids_and_labels_from_elements_and_element_maps() {
    let g = docs();
    same(
        &g,
        r#"g.V().elementMap().project("i", "l", "n").by(T.id).by(T.label).by("name").toList()"#,
        r#"g.v().element_map().project("i", "l", "n").by(T.id).by(T.label).by("name").to_list()"#,
        &[
            r#"#{"i": "1", "l": "doc", "n": "d1"}"#,
            r#"#{"i": "2", "l": "doc", "n": "d2"}"#,
        ],
    );
    same(
        &g,
        r#"g.V().project("m").by(__.elementMap()).select("m").project("i", "l").by(T.id).by(T.label).toList()"#,
        r#"g.v().project("m").by(__.element_map()).select("m").project("i", "l").by(T.id).by(T.label).to_list()"#,
        &[r#"#{"i": "1", "l": "doc"}"#, r#"#{"i": "2", "l": "doc"}"#],
    );
    same(
        &g,
        r#"g.E().project("i", "l").by(T.id).by(T.label).toList()"#,
        r#"g.e().project("i", "l").by(T.id).by(T.label).to_list()"#,
        &[r#"#{"i": "1", "l": "ref"}"#],
    );
    // a map without id/label entries gives an unproductive token by()
    same(
        &g,
        r#"g.V().valueMap("name").project("i", "l").by(T.id).by(T.label).toList()"#,
        r#"g.v().value_map("name").project("i", "l").by(T.id).by(T.label).to_list()"#,
        &["#{}", "#{}"],
    );
    // a vertex property element is labelled by its key
    same(
        &g,
        r#"g.V().has("name", "d1").properties("meta").project("l").by(T.label).toList()"#,
        r#"g.v().has("name", "d1").properties("meta").project("l").by(T.label).to_list()"#,
        &[r#"#{"l": "meta"}"#],
    );
}

#[test]
fn a_token_by_on_a_value_that_cannot_carry_it_fails() {
    let g = docs();
    for script in [
        r#"g.V().values("name").project("i").by(T.id).toList()"#,
        r#"g.V().values("name").fold().project("l").by(T.label).toList()"#,
    ] {
        let err = eval_value(g.clone(), script).expect_err(script).to_string();
        assert!(
            err.contains("T.id") || err.contains("T.label"),
            "{script}: {err}"
        );
    }
}

#[test]
fn an_injected_map_projects_by_key() {
    let g = docs();
    same(
        &g,
        r#"g.inject(#{a: 1, b: #{c: 2}}).project("x", "y", "z").by("a").by(jpath("b.c")).by("nope").toList()"#,
        r#"g.inject(#{a: 1, b: #{c: 2}}).project("x", "y", "z").by("a").by(jpath("b.c")).by("nope").to_list()"#,
        &[r#"#{"x": 1, "y": 2}"#],
    );
}

#[test]
fn a_column_by_orders_by_the_keys_or_values_of_object_properties() {
    let g = docs();
    // keys: d1 [a, b], d2 [b]; [b] sorts after [a, b]
    same(
        &g,
        r#"g.V().properties("meta").order().by(Column.keys, Order.desc).values("b").toList()"#,
        r#"g.v().properties("meta").order().by(Column::keys, Order.desc).values("b").to_list()"#,
        &["y", "x"],
    );
    same(
        &g,
        r#"g.V().properties("meta").order().by(Column.keys, Order.asc).values("b").toList()"#,
        r#"g.v().properties("meta").order().by(Column::keys, Order.asc).values("b").to_list()"#,
        &["x", "y"],
    );
}
