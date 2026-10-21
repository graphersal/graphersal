//! Equality between the current object and a labelled one is logical, not structural: a
//! computed value (from `inject`, `constant`, `math`) equals a property read lazily
//! (`values("age")`) in `where(P.eq("x"))`, `where(P.neq("x"))`, `where(__..as("x"))`,
//! `where(P.eq("x")).by(..)` and `where(P.within("side effect"))`, in both orders; numbers
//! compare by value (`1` equals `1.0`) as `P.eq` does for a literal; elements by identity.
#![cfg(feature = "script")]

use crate::script_host::eval_value;
use graphersal::prelude::*;
use std::sync::Arc;

fn run(script: &str) -> rhai::Dynamic {
    eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
}

fn sorted(script: &str) -> Vec<String> {
    let mut out: Vec<String> = run(script)
        .into_array()
        .unwrap_or_else(|_| panic!("{script}: not a list"))
        .into_iter()
        .map(|v| v.to_string())
        .collect();
    out.sort();
    out
}

#[test]
fn computed_label_equals_a_lazy_property() {
    for query in [
        r#"g.inject(29).as("x").V().values("age").where(P.eq("x")).toList()"#,
        r#"g.inject(29).as("x").v().values("age").where(P.eq("x")).to_list()"#,
    ] {
        assert_eq!(sorted(query), ["29"], "{query}");
    }
}

#[test]
fn lazy_label_equals_a_computed_value() {
    // The labelled side is the lazy property, the current object the computed value.
    let query = r#"g.V().has("name", "marko").values("age").as("x").constant(29).where(P.eq("x")).toList()"#;
    assert_eq!(sorted(query), ["29"]);
    let query = r#"g.V().has("name", "marko").values("age").as("x").math("_ + 1").where(P.eq("x")).toList()"#;
    assert!(sorted(query).is_empty());
}

#[test]
fn neq_excludes_the_equal_lazy_property() {
    let query = r#"g.inject(29).as("x").V().values("age").where(P.neq("x")).toList()"#;
    assert_eq!(sorted(query), ["27", "32", "35"]);
}

#[test]
fn integer_and_float_are_equal_as_for_a_literal() {
    let query = r#"g.inject(29.0).as("x").V().values("age").where(P.eq("x")).toList()"#;
    assert_eq!(sorted(query), ["29"]);
    // The same equality `is()` applies to a literal operand.
    let query = r#"g.V().values("age").is(P.eq(29.0)).toList()"#;
    assert_eq!(sorted(query), ["29"]);
    let query = r#"g.inject(1).as("x").inject(1.0).where(P.eq("x")).toList()"#;
    assert_eq!(run(query).into_array().unwrap().len(), 1);
}

#[test]
fn two_lazy_properties_of_different_elements_compare_by_value() {
    let query = r#"g.V().has("name", "lop").values("lang").as("x").V().has("name", "ripple").values("lang").where(P.eq("x")).toList()"#;
    assert_eq!(sorted(query), ["java"]);
}

#[test]
fn where_end_label_compares_logically() {
    for query in [
        r#"g.inject(29).as("x").V().where(__.values("age").as("x")).values("name").toList()"#,
        r#"g.inject(29).as("x").v().where(__.values("age").as("x")).values("name").to_list()"#,
    ] {
        assert_eq!(sorted(query), ["marko"], "{query}");
    }
    let query =
        r#"g.inject(29.0).as("x").V().where(__.values("age").as("x")).values("name").toList()"#;
    assert_eq!(sorted(query), ["marko"]);
}

#[test]
fn where_by_compares_logically() {
    let query =
        r#"g.inject(29).as("x").V().where(P.eq("x")).by("age").by().values("name").toList()"#;
    assert_eq!(sorted(query), ["marko"]);
    let query =
        r#"g.inject(29).as("x").V().where(P.neq("x")).by("age").by().values("name").toList()"#;
    // lop and ripple have no age: the unproductive `by("age")` drops them.
    assert_eq!(sorted(query), ["josh", "peter", "vadas"]);
}

#[test]
fn select_then_where_compares_logically() {
    let query =
        r#"g.inject(29).as("x").V().as("p").select("p").by("age").where(P.eq("x")).toList()"#;
    assert_eq!(sorted(query), ["29"]);
}

#[test]
fn vertices_compare_by_identity() {
    let query = r#"g.V().as("a").out().in().where(P.eq("a")).dedup().values("name").toList()"#;
    assert_eq!(sorted(query), ["josh", "marko", "peter"]);
    let query = r#"g.V().has("name", "marko").as("a").V().where(P.neq("a")).count().next()"#;
    assert_eq!(run(query).as_int().unwrap(), 5);
    // A vertex never equals a value.
    let query = r#"g.inject(1).as("x").V().where(P.eq("x")).count().next()"#;
    assert_eq!(run(query).as_int().unwrap(), 0);
}

#[test]
fn nested_values_compare_by_value() {
    let query = r#"g.inject(1).constant([1, [2, 3]]).as("x").constant([1, [2, 3]]).where(P.eq("x")).toList()"#;
    assert_eq!(run(query).into_array().unwrap().len(), 1);
    let query =
        r#"g.inject(1).constant([1, 2]).as("x").constant([1, 3]).where(P.eq("x")).toList()"#;
    assert!(run(query).into_array().unwrap().is_empty());
    let query =
        r#"g.inject(1).constant(#{a: [1]}).as("x").constant(#{a: [1]}).where(P.eq("x")).toList()"#;
    assert_eq!(run(query).into_array().unwrap().len(), 1);
    // A list never equals a scalar.
    let query = r#"g.inject(1).constant([29]).as("x").V().values("age").where(P.eq("x")).toList()"#;
    assert!(sorted(query).is_empty());
}

#[test]
fn within_a_side_effect_compares_logically() {
    let query = r#"g.inject(29.0).aggregate("a").V().values("age").where(P.within("a")).toList()"#;
    assert_eq!(sorted(query), ["29"]);
    let query = r#"g.V().has("name", "lop").values("lang").aggregate("a").V().has("name", "ripple").values("lang").where(P.within("a")).toList()"#;
    assert_eq!(sorted(query), ["java"]);
    let query = r#"g.V().has("name", "lop").values("lang").aggregate("a").V().has("name", "ripple").values("lang").where(P.without("a")).toList()"#;
    assert!(sorted(query).is_empty());
}
