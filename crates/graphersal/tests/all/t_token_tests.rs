//! Integration tests for TinkerPop's `T` token in the Rhai DSL: `T.id`/`T.label` (and the
//! `T::id`/`T::label` static-module spelling) work in every position `By.Id`/`By.Label` work,
//! with identical results, and `has(T.id, ..)`/`has(T.label, ..)` filter like
//! `has_id(..)`/`has_label(..)`.
#![cfg(feature = "script")]

use std::sync::Arc;

use crate::script_host::rhai::{Dynamic, EvalAltResult};
use graphersal::prelude::*;

/// Evaluates `script` against `graph` and renders the result the way the REPL prints plain Rhai
/// values (`{:?}`), so two scripts can be compared by their output.
fn run_on(graph: fn() -> Graph, script: &str) -> String {
    let value: Dynamic = crate::script_host::eval_value(Arc::new(graph()), script)
        .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
    format!("{value:?}")
}

fn modern(script: &str) -> String {
    run_on(GraphSource::tinkerpop_modern, script)
}

fn tree(script: &str) -> String {
    run_on(GraphSource::file_tree, script)
}

/// The runtime error message of a failing `script` on the modern graph.
fn modern_err(script: &str) -> String {
    match crate::script_host::eval_value(Arc::new(GraphSource::tinkerpop_modern()), script) {
        Ok(value) => panic!("`{script}` unexpectedly succeeded with {value:?}"),
        Err(err) => match *err {
            EvalAltResult::ErrorRuntime(message, ..) => message.to_string(),
            other => other.to_string(),
        },
    }
}

/// Asserts that the `T.*` script, its `T::*` spelling and the `By.*` script all print
/// `expected`.
fn assert_t_equals_by(run: fn(&str) -> String, t_script: &str, expected: &str) {
    let by_script = t_script
        .replace("T.id", "By.Id")
        .replace("T.label", "By.Label");
    let colon_script = t_script
        .replace("T.id", "T::id")
        .replace("T.label", "T::label");
    assert_eq!(run(t_script), expected, "`{t_script}`");
    assert_eq!(run(&colon_script), expected, "`{colon_script}`");
    assert_eq!(run(&by_script), expected, "`{by_script}`");
}

#[test]
fn t_tokens_are_the_by_tokens() {
    assert_eq!(modern(r#"T.id.enum_type"#), r#""Id""#);
    assert_eq!(modern(r#"T.label.enum_type"#), r#""Label""#);
    assert_eq!(modern(r#"T::id.enum_type"#), r#""Id""#);
    assert_eq!(modern(r#"T::label.enum_type"#), r#""Label""#);
}

#[test]
fn order_by_t_id_and_t_label() {
    assert_t_equals_by(
        modern,
        r#"g.v().order().by(T.id, Order.desc).id().to_list()"#,
        r#"["6", "5", "4", "3", "2", "1"]"#,
    );
    assert_t_equals_by(
        modern,
        r#"g.v().order().by(T.label, Order.desc).by(T.id).id().to_list()"#,
        r#"["3", "5", "1", "2", "4", "6"]"#,
    );
}

#[test]
fn group_and_group_count_by_t_label() {
    for step in ["group_count", "groupCount"] {
        assert_t_equals_by(
            modern,
            &format!(r#"g.v().{step}().by(T.label).next()"#),
            r#"#{"person": 4, "software": 2}"#,
        );
    }
    assert_t_equals_by(
        modern,
        r#"g.v().group().by(T.label).by(T.id).next()"#,
        r#"#{"person": ["1", "2", "4", "6"], "software": ["3", "5"]}"#,
    );
}

#[test]
fn project_select_path_and_tree_by_t_tokens() {
    assert_t_equals_by(
        modern,
        r#"g.v("1").project("i", "l").by(T.id).by(T.label).to_list()"#,
        r#"[#{"i": "1", "l": "person"}]"#,
    );
    assert_t_equals_by(
        modern,
        r#"g.v("1").as_("a").out("created").as_("b").select("a", "b").by(T.id).by(T.label).to_list()"#,
        r#"[#{"a": "1", "b": "software"}]"#,
    );
    assert_t_equals_by(
        modern,
        r#"g.v("1").out("created").path().by(T.id).by(T.label).to_list()"#,
        r#"[["1", "software"]]"#,
    );
    assert_t_equals_by(
        modern,
        r#"g.v("1").out("created").tree().by(T.label).next()"#,
        r#"#{"person": #{"software": #{}}}"#,
    );
}

#[test]
fn glob_path_by_t_tokens_in_both_spellings() {
    {
        let step = "glob_path";
        assert_t_equals_by(
            tree,
            &format!(
                r#"g.v("root").{step}("**/*\\/util\\/*").by(T.id).order().by(T.id).id().to_list()"#
            ),
            r#"["root/src/util/io.rs", "root/src/util/mod.rs"]"#,
        );
        assert_t_equals_by(
            tree,
            &format!(
                r#"g.v("root").{step}("**/symlink").by(T.label).order().by(T.id).id().to_list()"#
            ),
            r#"["root/latest", "root/y/loop"]"#,
        );
    }
}

#[test]
fn has_t_label_equals_has_label() {
    let expected = r#"["marko", "vadas", "josh", "peter"]"#;
    assert_eq!(
        modern(r#"g.v().has(T.label, "person").values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().has(T::label, "person").values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().has(T.label, P.eq("person")).values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().has_label("person").values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().hasLabel("person").values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().has(T.label, P.within(["software", "nothing"])).id().to_list()"#),
        r#"["3", "5"]"#
    );
    assert_eq!(
        modern(r#"g.v().where(__.has(T.label, "software")).id().to_list()"#),
        r#"["3", "5"]"#
    );
}

#[test]
fn has_t_id_equals_has_id() {
    let expected = r#"["marko"]"#;
    assert_eq!(
        modern(r#"g.v().has(T.id, "1").values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().has(T.id, 1).values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().has(T::id, P.eq("1")).values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().has_id("1").values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().hasId("1").values("name").to_list()"#),
        expected
    );
    assert_eq!(
        modern(r#"g.v().has(T.id, P.within(["1", "2"])).values("name").to_list()"#),
        r#"["marko", "vadas"]"#
    );
    assert_eq!(
        modern(r#"g.e().has(T.id, "7").label().to_list()"#),
        modern(r#"g.e().hasId("7").label().to_list()"#)
    );
}

#[test]
fn has_t_with_an_unsupported_value_is_a_script_error() {
    // Any predicate is accepted now (`has(T.label, P.gt("a"))` is `hasLabel(P.gt("a"))`).
    assert_eq!(
        modern(r#"g.v().has(T.label, P.gt("a")).count().next()"#),
        "6"
    );
    let err = modern_err(r#"g.v().has(T.id, ["1", "2"]).to_list()"#);
    assert!(err.contains("use P.within([..])"), "{err}");
    let err = modern_err(r#"g.v().has(By.Count, 1).to_list()"#);
    assert!(err.contains("a property key, T.id or T.label"), "{err}");
}

#[test]
fn t_key_and_t_value_read_a_property_element() {
    // `T.key`/`T.value` (TinkerPop 3.8 `Property` tokens) project a `properties()` value.
    assert_eq!(
        modern(r#"g.V("1").properties("name").as("a").select("a").by(T.key).to_list()"#),
        r#"["name"]"#
    );
    assert_eq!(
        modern(r#"g.V("1").properties("name").as("a").select("a").by(T::value).to_list()"#),
        r#"["marko"]"#
    );
    assert_eq!(
        modern(r#"g.V().properties().order().by(T.key, Order.desc).key().dedup().to_list()"#),
        r#"["name", "lang", "age"]"#
    );
}
