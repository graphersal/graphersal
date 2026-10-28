//! Integration tests for `valueMap()`: as in TinkerPop, every property value is wrapped in a list
//! (`{name: [marko], age: [29]}`), `id`/`label` are left out, and `by()` is a traversal applied to
//! each value list (`valueMap().by(__.unfold())` returns the plain values). Every expected result
//! is what TinkerPop returns for the same query on the modern graph.
//!
//! This engine's modern graph numbers its edges from `0`, so TinkerPop's edge `7` (marko knows
//! vadas, `weight` 0.5) is edge `"0"` here.

#[cfg(feature = "script")]
use std::sync::Arc;

use graphersal::prelude::*;

/// A one-element list holding `value`, the shape `valueMap()` gives every property.
fn list(value: impl Into<ElementProperty>) -> ElementProperty {
    ElementProperty::Array(vec![value.into()])
}

/// The single map a traversal returned.
fn single_map(results: Vec<ElementProperty>) -> PropertyMap {
    assert_eq!(results.len(), 1, "expected one map, got {results:?}");
    match results.into_iter().next() {
        Some(ElementProperty::Object(map)) => *map,
        other => panic!("expected a map, got {other:?}"),
    }
}

/// Asserts that `err` is rooted in `InvalidModulator` for `value_map` at `position`.
fn assert_invalid_value_map_modulator(err: &TraverserError, position: usize) {
    match err.root_cause() {
        TraverserError::InvalidModulator {
            step,
            position: actual_position,
            ..
        } => {
            assert_eq!(step, "value_map", "wrong step in: {err}");
            assert_eq!(*actual_position, position, "wrong position in: {err}");
        }
        other => panic!("expected InvalidModulator, got: {other}"),
    }
}

/// TinkerPop: `g.V(1).valueMap()` is `{name: [marko], age: [29]}`.
#[test]
fn value_map_wraps_every_value_in_a_list() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let map = single_map(lock.traversal().v("1").value_map(None).to_list().unwrap());
    assert_eq!(map.len(), 2);
    assert_eq!(map.get("name"), Some(&list("marko")));
    assert_eq!(map.get("age"), Some(&list(29i64)));
}

/// TinkerPop: `g.V(1).valueMap("name")` is `{name: [marko]}`; the one-key, several-key and
/// key-list arities agree.
#[test]
fn value_map_with_keys_keeps_only_those_keys() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let map = single_map(lock.traversal().v("1").value_map("name").to_list().unwrap());
    assert_eq!(map.len(), 1);
    assert_eq!(map.get("name"), Some(&list("marko")));

    let map = single_map(
        lock.traversal()
            .v("1")
            .value_map(vec!["name", "age"])
            .to_list()
            .unwrap(),
    );
    assert_eq!(map.len(), 2);
    assert_eq!(map.get("name"), Some(&list("marko")));
    assert_eq!(map.get("age"), Some(&list(29i64)));
}

/// TinkerPop: `g.V(1).valueMap().by(unfold())` is `{name: marko, age: 29}`.
#[test]
fn by_unfold_unwraps_the_values() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let map = single_map(
        lock.traversal()
            .v("1")
            .value_map(None)
            .by(__::unfold())
            .to_list()
            .unwrap(),
    );
    assert_eq!(map.len(), 2);
    assert_eq!(map.get("name"), Some(&ElementProperty::from("marko")));
    assert_eq!(map.get("age").and_then(|v| v.as_i64()), Some(29));
}

/// TinkerPop's productive `by()`: a key whose `by()` traversal yields nothing is left out.
#[test]
fn an_unproductive_by_drops_the_key() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let map = single_map(
        lock.traversal()
            .v("1")
            .value_map(None)
            .by(__::unfold().is(29i64))
            .to_list()
            .unwrap(),
    );
    assert_eq!(map.len(), 1, "{map:?}");
    assert_eq!(map.get("age").and_then(|v| v.as_i64()), Some(29));
}

/// TinkerPop: `g.E(7).valueMap()` is `{weight: [0.5]}`.
#[test]
fn edge_value_map_wraps_the_weight() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let map = single_map(lock.traversal().e("0").value_map(None).to_list().unwrap());
    assert_eq!(map.len(), 1);
    assert_eq!(map.get("weight"), Some(&list(0.5f64)));
}

/// A vertex without properties has an empty value map.
#[test]
fn a_vertex_without_properties_has_an_empty_map() {
    let graph = GraphSource::empty();
    graph.write().traversal_mut().add_v("thing").next().unwrap();
    let lock = graph.read();
    let map = single_map(lock.traversal().v(None).value_map(None).to_list().unwrap());
    assert!(map.is_empty(), "{map:?}");
}

/// `by()` is applied to a value list, so only a traversal fits; a label list and every
/// projection kind that cannot take a list are rejected when the step executes.
#[test]
fn a_by_that_cannot_take_a_list_is_rejected() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .value_map(None)
        .by(["person"])
        .to_list()
        .expect_err("labels");
    assert_invalid_value_map_modulator(&err, 1);
    let err = lock
        .traversal()
        .v(None)
        .value_map(None)
        .by("name")
        .to_list()
        .expect_err("property");
    assert_invalid_value_map_modulator(&err, 1);
    let err = lock
        .traversal()
        .v(None)
        .value_map(None)
        .by(By::Id)
        .to_list()
        .expect_err("id");
    assert_invalid_value_map_modulator(&err, 1);
    let err = lock
        .traversal()
        .v(None)
        .value_map(None)
        .by(__::unfold())
        .by(By::Label)
        .to_list()
        .expect_err("label");
    assert_invalid_value_map_modulator(&err, 2);
}

/// Rhai, both spellings: the rendered script value of each query.
#[cfg(feature = "script")]
mod rhai {
    use super::*;

    /// The `Debug` rendering of a script value evaluated on `graph`.
    fn eval_on(graph: Graph, script: &str) -> String {
        let value = crate::script_host::eval_value(Arc::new(graph), script)
            .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
        format!("{value:?}")
    }

    fn modern(script: &str) -> String {
        eval_on(GraphSource::tinkerpop_modern(), script)
    }

    #[test]
    fn value_map_shapes_in_both_spellings() {
        let cases = [
            (
                r#"g.V("1").valueMap().toList()"#,
                r#"[#{"age": [29], "name": ["marko"]}]"#,
            ),
            (
                r#"g.v("1").value_map().to_list()"#,
                r#"[#{"age": [29], "name": ["marko"]}]"#,
            ),
            (
                r#"g.V("1").valueMap("name").toList()"#,
                r#"[#{"name": ["marko"]}]"#,
            ),
            (
                r#"g.v("1").value_map("name").to_list()"#,
                r#"[#{"name": ["marko"]}]"#,
            ),
            (
                r#"g.V("1").valueMap("name", "age").toList()"#,
                r#"[#{"age": [29], "name": ["marko"]}]"#,
            ),
            (
                r#"g.v("1").value_map(["name", "age"]).to_list()"#,
                r#"[#{"age": [29], "name": ["marko"]}]"#,
            ),
            (
                r#"g.V("1").valueMap().by(__.unfold()).toList()"#,
                r#"[#{"age": 29, "name": "marko"}]"#,
            ),
            (
                r#"g.v("1").value_map().by(__.unfold()).to_list()"#,
                r#"[#{"age": 29, "name": "marko"}]"#,
            ),
            (r#"g.E("0").valueMap().toList()"#, r#"[#{"weight": [0.5]}]"#),
            (
                r#"g.e("0").value_map().to_list()"#,
                r#"[#{"weight": [0.5]}]"#,
            ),
        ];
        for (script, expected) in cases {
            assert_eq!(modern(script), expected, "`{script}`");
        }
    }

    #[test]
    fn a_vertex_without_properties_has_an_empty_map() {
        for script in [
            r#"g.addV("thing").next(); g.V().valueMap().toList()"#,
            r#"g.add_v("thing").next(); g.v().value_map().to_list()"#,
        ] {
            assert_eq!(eval_on(GraphSource::empty(), script), "[#{}]", "`{script}`");
        }
    }

    #[test]
    fn a_label_list_by_is_rejected() {
        for script in [
            r#"g.V().valueMap().by(["person"]).toList()"#,
            r#"g.v().value_map().by(["person"]).to_list()"#,
        ] {
            let err =
                crate::script_host::eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
                    .expect_err(script)
                    .to_string();
            assert!(
                err.contains(
                    "`value_map()` cannot use its by() #1: a vertex-label list is not a projection"
                ),
                "`{script}`: {err}"
            );
        }
    }
}
