//! `group()` / `groupCount()` / `unfold()` / `tree()` keep the type of their keys: a materialized
//! map with a non-string key is an `ElementProperty::Map`, a Rhai `ScriptMap` and a `g:Map` in JSON,
//! while a map with only string keys stays an `Object` / `rhai::Map` with plain JSON.
#![cfg(feature = "script")]

use crate::script_host::{ScriptMap, eval_value, eval_value_with_params};
use graphersal::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn run(script: &str) -> rhai::Dynamic {
    eval_value(modern(), script).unwrap_or_else(|e| panic!("`{script}` failed: {e}"))
}

fn int(n: i64) -> ElementProperty {
    ElementProperty::from(n)
}

fn text(s: &str) -> ElementProperty {
    ElementProperty::from(s)
}

#[test]
fn group_count_by_int_key_is_indexable_by_that_int() {
    assert_eq!(
        run(r#"g.V().groupCount().by("age").next()[29]"#)
            .as_int()
            .unwrap(),
        1
    );
    assert!(run(r#"g.V().groupCount().by("age").next()"#).is::<ScriptMap>());
    // keys() carries the type: ints, not strings.
    let keys = run(r#"g.V().groupCount().by("age").next().keys()"#)
        .into_array()
        .unwrap();
    assert!(keys.iter().all(|k| k.is_int()), "{keys:?}");
    assert_eq!(keys.len(), 4);
}

#[test]
fn group_by_keeps_every_key_kind() {
    let rust = |f: fn(
        GraphTraversalSource<'_, &TraversalGraph>,
    ) -> TraverserResult<Vec<ElementProperty>>| {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        f(lock.traversal()).unwrap()
    };
    // List key.
    let res = rust(|g| {
        g.v(None)
            .has_label("person")
            .group_count()
            .by(__::values("age").fold())
            .to_list()
    });
    let ElementProperty::Map(map) = &res[0] else {
        panic!("expected a map, got {:?}", res[0])
    };
    assert_eq!(
        map.get(&ElementProperty::Array(vec![int(29)])),
        Some(&int(1))
    );
    // Float key stays a float.
    let res = rust(|g| {
        g.inject(vec![ElementProperty::from(2.5f64)])
            .group_count()
            .to_list()
    });
    let ElementProperty::Map(map) = &res[0] else {
        panic!("expected a map, got {:?}", res[0])
    };
    assert_eq!(map.get(&ElementProperty::from(2.5f64)), Some(&int(1)));
    // Vertex key stays a vertex.
    let res = rust(|g| g.v("1").group_count().by(__::identity()).to_list());
    let ElementProperty::Map(map) = &res[0] else {
        panic!("expected a map, got {:?}", res[0])
    };
    let (key, value) = map.iter().next().unwrap();
    assert!(matches!(key, ElementProperty::Vertex(_)), "{key:?}");
    assert_eq!(value, &int(1));
}

#[test]
fn uuid_key_stays_a_uuid() {
    let uid = ElementProperty::uuid_from_string("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .inject(vec![uid.clone()])
        .group_count()
        .to_list()
        .unwrap();
    let ElementProperty::Map(map) = &res[0] else {
        panic!("expected a map, got {:?}", res[0])
    };
    assert_eq!(map.get(&uid), Some(&int(1)));
    assert_eq!(map.get_str("550e8400-e29b-41d4-a716-446655440000"), None);
}

#[cfg(feature = "serde")]
#[test]
fn json_output_is_tagged_only_for_typed_keys() {
    let typed =
        run(r#"let out = g.V().groupCount().by("age").to_json(); out.to_string()"#).to_string();
    assert!(typed.contains("g:Map"), "{typed}");
    let plain =
        run(r#"let out = g.V().groupCount().by("name").to_json(); out.to_string()"#).to_string();
    assert!(
        !plain.contains("g:Map") && plain.contains("\"marko\""),
        "{plain}"
    );
}

#[test]
fn string_key_group_count_is_unchanged() {
    let value = run(r#"g.V().groupCount().by("name").next()"#);
    assert!(value.is_map(), "a string-keyed result stays a rhai::Map");
    let map = value.cast::<rhai::Map>();
    assert_eq!(map["marko"].as_int().unwrap(), 1);
    // The label-keyed pushdown (`GroupCountPushdown`) stays an object too.
    let value = run(r#"g.V().groupCount().by(T.label).next()"#);
    assert!(value.is_map(), "{value:?}");
}

#[test]
fn nested_group_materializes_recursively() {
    let inner = run(
        r#"g.V().hasLabel("person").group().by(T.label).by(__.groupCount().by("age")).next()["person"]"#,
    );
    let inner = inner
        .try_cast::<ScriptMap>()
        .expect("the inner map is typed");
    assert_eq!(inner.as_value_map().get(&int(29)), Some(&int(1)));
}

#[test]
fn unfold_and_select_keys_give_typed_keys() {
    let keys = run(r#"g.V().groupCount().by("age").unfold().select(Column.keys).toList()"#)
        .into_array()
        .unwrap();
    assert_eq!(keys.len(), 4);
    assert!(keys.iter().all(|k| k.is_int()), "{keys:?}");
    let entry = run(r#"g.V().groupCount().by("age").unfold().next()"#);
    assert!(entry.is::<ScriptMap>(), "a one-entry map keeps its int key");
    let keys = run(r#"g.V().groupCount().by("age").select(Column.keys).next()"#)
        .into_array()
        .unwrap();
    assert!(keys.iter().all(|k| k.is_int()), "{keys:?}");
}

#[test]
fn dedup_of_equal_maps_built_in_different_orders() {
    let a = ElementProperty::from_entries([(int(1), text("x")), (text("k"), int(2))]);
    let b = ElementProperty::from_entries([(text("k"), int(2)), (int(1), text("x"))]);
    let params = HashMap::from([("a".to_string(), a), ("b".to_string(), b)]);
    let count =
        eval_value_with_params(modern(), "g.inject(a, b).dedup().count().next()", params).unwrap();
    assert_eq!(count.as_int().unwrap(), 1);
}

#[test]
fn order_local_over_mixed_key_kinds_does_not_panic() {
    let value = run(
        r#"g.V().group().by(__.union(__.values("age"), __.values("name"))).by("name").order(Scope.local).next()"#,
    );
    assert!(value.is::<ScriptMap>());
    let keys = run(
        r#"g.V().group().by(__.union(__.values("age"), __.values("name"))).by("name").order(Scope.local).next().keys()"#,
    )
    .into_array()
    .unwrap();
    assert_eq!(keys.len(), 6);
}

#[cfg(feature = "display")]
#[test]
fn display_renders_keys_through_display_text() {
    let table =
        run(r#"let out = g.V().groupCount().by("age").to_table(); out.to_string()"#).to_string();
    assert!(table.contains("29") && table.contains("35"), "{table}");
    let profile =
        run(r#"let out = g.V().groupCount().by("age").profile(); out.to_string()"#).to_string();
    assert!(profile.contains("group_count"), "{profile}");
    let tree =
        run(r#"let out = g.V("1").out("knows").tree().to_table(); out.to_string()"#).to_string();
    assert!(tree.contains("v[1]"), "{tree}");
}

#[test]
fn tree_keys_stay_vertices() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .out("knows")
        .tree(None)
        .to_list()
        .unwrap();
    let ElementProperty::Map(root) = &res[0] else {
        panic!("expected a map, got {:?}", res[0])
    };
    assert!(root.keys().all(|k| matches!(k, ElementProperty::Vertex(_))));
}

#[test]
fn bulk_merge_does_not_change_a_group_count_over_repeat() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let run = |merge: bool| {
        lock.traversal()
            .with("bulk.merge", merge)
            .v(None)
            .repeat(__::out(None))
            .times(2)
            .group_count()
            .by("name")
            .to_list()
            .unwrap()
    };
    assert_eq!(run(false), run(true));
    let typed = |merge: bool| {
        lock.traversal()
            .with("bulk.merge", merge)
            .v(None)
            .repeat(__::both(None))
            .times(2)
            .group_count()
            .by("age")
            .to_list()
            .unwrap()
    };
    assert_eq!(typed(false), typed(true));
}

#[test]
fn a_constant_map_is_refused_as_a_property_value() {
    let err = eval_value(
        modern(),
        r#"let m = g.V().groupCount().by("age").next(); g.V("1").property("m", __.constant(m)).next()"#,
    )
    .expect_err("a map with a non-string key is not storable");
    let text = err.to_string();
    assert!(text.contains("non-string key"), "{text}");
}

#[test]
fn string_keyed_results_equal_across_random_inputs() {
    // A group over strings must stay an `Object` whatever the input mix.
    for n in 0..20i64 {
        let names: Vec<String> = (0..n).map(|i| format!("k{}", i % 5)).collect();
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let res = lock
            .traversal()
            .inject(names.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            .unfold()
            .group_count()
            .to_list()
            .unwrap();
        match &res[0] {
            ElementProperty::Object(map) => {
                let total: i64 = map.values().map(|v| v.as_i64().unwrap()).sum();
                assert_eq!(total, n);
            }
            other => panic!("expected an object, got {other:?}"),
        }
    }
}
