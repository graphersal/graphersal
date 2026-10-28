//! Integration tests for the `Cardinality`, `Direction` and `Merge` token classes in the Rhai DSL
//! (dot form `Class.member` and static-module form `Class::member`).
#![cfg(feature = "script")]

use std::sync::Arc;

use crate::script_host::{eval, eval_value, rhai::Dynamic};
use graphersal::prelude::*;

fn value(script: &str) -> Dynamic {
    eval_value(Arc::new(GraphSource::tinkerpop_modern()), script).unwrap()
}

fn rendered(script: &str) -> String {
    eval(Arc::new(GraphSource::tinkerpop_modern()), script).unwrap()
}

#[test]
fn cardinality_members_in_both_forms() {
    for (member, expected) in [
        ("single", Cardinality::Single),
        ("list", Cardinality::List),
        ("set", Cardinality::Set),
    ] {
        for script in [
            format!("Cardinality.{member}"),
            format!("Cardinality::{member}"),
        ] {
            assert_eq!(
                value(&script).try_cast::<Cardinality>(),
                Some(expected),
                "{script}"
            );
        }
    }
}

#[test]
fn direction_members_in_both_forms() {
    for (member, expected) in [
        ("OUT", Direction::Out),
        ("IN", Direction::In),
        ("BOTH", Direction::Both),
        ("from", Direction::Out),
        ("to", Direction::In),
    ] {
        for script in [
            format!("Direction.{member}"),
            format!("Direction::{member}"),
        ] {
            assert_eq!(
                value(&script).try_cast::<Direction>(),
                Some(expected),
                "{script}"
            );
        }
    }
}

#[test]
fn merge_members_in_both_forms() {
    for (member, expected) in [
        ("onCreate", Merge::OnCreate),
        ("onMatch", Merge::OnMatch),
        ("outV", Merge::OutV),
        ("inV", Merge::InV),
    ] {
        for script in [format!("Merge.{member}"), format!("Merge::{member}")] {
            assert_eq!(
                value(&script).try_cast::<Merge>(),
                Some(expected),
                "{script}"
            );
        }
    }
}

#[test]
fn cardinality_call_wraps_a_value() {
    for script in [
        "type_of(Cardinality.single(5))",
        "type_of(Cardinality::single(5))",
        "type_of(Cardinality.list(5))",
        "type_of(Cardinality::set(5))",
    ] {
        assert_eq!(
            value(script).into_string().unwrap(),
            "CardinalityValue",
            "{script}"
        );
    }
    assert_eq!(
        value("Cardinality.single(5).to_string()")
            .into_string()
            .unwrap(),
        "Cardinality.single(5)"
    );
    assert_eq!(
        value("Cardinality::set(\"x\").to_string()")
            .into_string()
            .unwrap(),
        "Cardinality.set(\"x\")"
    );
}

#[test]
fn tokens_render_with_their_gremlin_spelling() {
    assert_eq!(
        value("Direction.OUT.to_string()").into_string().unwrap(),
        "Direction.OUT"
    );
    assert_eq!(
        value("Direction::from.to_string()").into_string().unwrap(),
        "Direction.OUT"
    );
    assert_eq!(
        value("Merge.onCreate.to_string()").into_string().unwrap(),
        "Merge.onCreate"
    );
    assert_eq!(
        value("Cardinality.single.to_string()")
            .into_string()
            .unwrap(),
        "Cardinality.single"
    );
    assert_eq!(rendered("Direction.BOTH"), "Direction.BOTH");
    assert_eq!(rendered("Merge::inV"), "Merge.inV");
    assert_eq!(
        value("type_of(Direction.IN)").into_string().unwrap(),
        "Direction"
    );
    assert_eq!(value("type_of(Merge.outV)").into_string().unwrap(), "Merge");
    assert_eq!(
        value("type_of(Cardinality.set)").into_string().unwrap(),
        "Cardinality"
    );
}

#[test]
fn merge_vertex_placeholders_become_reserved_strings_as_values() {
    let map = value(r#"g.inject(1).constant(#{"out": Merge.outV, "in": Merge::inV}).next()"#)
        .try_cast::<crate::script_host::rhai::Map>()
        .unwrap();
    assert_eq!(map["out"].clone().into_string().unwrap(), "Merge.outV");
    assert_eq!(map["in"].clone().into_string().unwrap(), "Merge.inV");
    assert_eq!(
        value("g.inject(Merge.outV).next()").into_string().unwrap(),
        "Merge.outV"
    );
}

/// Documented fallback: other tokens have no value meaning, and the value conversion is
/// infallible, so they are kept as their Gremlin spelling (never an opaque Rust type name).
#[test]
fn other_tokens_as_values_keep_their_gremlin_spelling() {
    let map = value(
        r#"g.inject(1).constant(#{"d": Direction.OUT, "m": Merge.onCreate, "c": Cardinality.list, "v": Cardinality.single(5)}).next()"#,
    )
    .try_cast::<crate::script_host::rhai::Map>()
    .unwrap();
    assert_eq!(map["d"].clone().into_string().unwrap(), "Direction.OUT");
    assert_eq!(map["m"].clone().into_string().unwrap(), "Merge.onCreate");
    assert_eq!(map["c"].clone().into_string().unwrap(), "Cardinality.list");
    assert_eq!(
        map["v"].clone().into_string().unwrap(),
        "Cardinality.single(5)"
    );
}

#[test]
fn no_bare_token_identifiers() {
    for script in ["single", "OUT", "onCreate"] {
        assert!(
            eval_value(Arc::new(GraphSource::tinkerpop_modern()), script).is_err(),
            "{script}"
        );
    }
}
