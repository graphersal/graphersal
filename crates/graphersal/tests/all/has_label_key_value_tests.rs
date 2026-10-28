//! Integration tests for TinkerPop's three-argument `has(label, key, value)` /
//! `has(label, key, P)`, in the Rust API (`has_labeled`/`has_labeled_p`) and the Rhai DSL.

#[cfg(feature = "script")]
use std::sync::Arc;

#[cfg(feature = "script")]
use crate::script_host::{eval, eval_value};
use graphersal::prelude::*;

#[cfg(feature = "script")]
fn strings(script: &str) -> Vec<String> {
    let mut out: Vec<String> = eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
        .unwrap()
        .into_array()
        .unwrap()
        .into_iter()
        .map(|v| v.to_string())
        .collect();
    out.sort();
    out
}

#[cfg(feature = "script")]
fn int(script: &str) -> i64 {
    eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
        .unwrap()
        .as_int()
        .unwrap()
}

#[test]
fn rust_value_form() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let ages = lock
        .traversal()
        .v(None::<()>)
        .has_labeled("person", "name", "marko")
        .values("age")
        .to_list()
        .unwrap();
    assert_eq!(ages, vec![ElementProperty::Number(29.into())]);
    let none = lock
        .traversal()
        .v(None::<()>)
        .has_labeled("software", "name", "marko")
        .to_list()
        .unwrap();
    assert!(none.is_empty());
    let anon = lock
        .traversal()
        .v(None::<()>)
        .where_t(__::has_labeled("person", "name", "josh"))
        .values("age")
        .to_list()
        .unwrap();
    assert_eq!(anon, vec![ElementProperty::Number(32.into())]);
}

#[test]
fn rust_predicate_form() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let mut names = lock
        .traversal()
        .v(None::<()>)
        .has_labeled_p("person", "age", P::Gt(30))
        .values("name")
        .to_list()
        .unwrap();
    names.sort_by_key(|v| format!("{v:?}"));
    assert_eq!(
        names,
        vec![
            ElementProperty::String("josh".into()),
            ElementProperty::String("peter".into())
        ]
    );
}

#[test]
fn rust_label_is_pushed_into_the_source() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let profile = lock
        .traversal()
        .v(None::<()>)
        .has_labeled("person", "name", "marko")
        .count()
        .profile()
        .unwrap()
        .to_string();
    assert!(profile.contains(r#"v(labels: ["person"])"#), "{profile}");
    assert!(!profile.contains("has_label("), "{profile}");
}

#[cfg(feature = "script")]
#[test]
fn dsl_value_and_predicate_forms() {
    assert_eq!(
        strings(r#"g.V().has("person", "name", "marko").values("age").toList()"#),
        vec!["29"]
    );
    assert_eq!(
        int(r#"g.V().has("software", "name", "marko").count().next()"#),
        0
    );
    assert_eq!(
        strings(r#"g.V().has("person", "age", P.gt(30)).values("name").toList()"#),
        vec!["josh", "peter"]
    );
    assert_eq!(
        strings(r#"g.V().where(__.has("person", "age", P::lt(30))).values("name").to_list()"#),
        vec!["marko", "vadas"]
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_on_edges() {
    assert_eq!(
        int(r#"g.E().has("knows", "weight", 0.5).count().next()"#),
        1
    );
    assert_eq!(
        int(r#"g.E().has("created", "weight", P.gte(0.4)).count().next()"#),
        3
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_existing_forms_unchanged() {
    assert_eq!(int(r#"g.V().has("name", "marko").count().next()"#), 1);
    assert_eq!(int(r#"g.V().has(T.label, "person").count().next()"#), 4);
    assert_eq!(int(r#"g.V().has("age").count().next()"#), 4);
}

#[cfg(feature = "script")]
#[test]
fn dsl_label_is_pushed_into_the_source() {
    let profile = eval(
        Arc::new(GraphSource::tinkerpop_modern()),
        r#"g.V().has("person", "name", "marko").count().profile()"#,
    )
    .unwrap();
    assert!(profile.contains(r#"v(labels: ["person"])"#), "{profile}");
    assert!(profile.contains("has(\"name\""), "{profile}");
}
