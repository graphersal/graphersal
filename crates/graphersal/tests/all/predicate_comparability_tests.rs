//! TinkerPop comparability rules for `P` (`semantics/Comparability.feature`): numbers compare by
//! value across int/float, `NaN` is incomparable, values of different kinds are incomparable and
//! every ordering test on them is false (never an error, never an accidental true).
#![cfg(feature = "script")]

use crate::script_host::eval_value;
use graphersal::prelude::*;
use std::sync::Arc;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// Number of results of `g.inject(<value>).is(<predicate>)`.
fn kept(value: &str, predicate: &str) -> i64 {
    let script = format!(
        "let nan = 0.0 / 0.0; let inf = 1.0 / 0.0; \
         g.inject({value}).is({predicate}).toList().len()"
    );
    eval_value(modern(), &script)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
        .as_int()
        .unwrap()
}

#[test]
fn numbers_compare_by_value_across_int_and_float() {
    let cases = [
        ("1.0", "P.eq(1)", 1),
        ("1", "P.eq(1.0)", 1),
        ("1.0", "P.neq(1)", 0),
        ("1", "P.neq(1.0)", 0),
        ("1", "P.lt(1.5)", 1),
        ("2.0", "P.lt(2)", 0),
        ("2.0", "P.lte(2)", 1),
        ("2", "P.gt(1.5)", 1),
        ("2", "P.gte(2.0)", 1),
        ("1", "P.within([1.0, 5])", 1),
        ("1.0", "P.within([1, 5])", 1),
        ("2", "P.without([2.0])", 0),
        ("1", "P.between(0.5, 3)", 1),
        ("3", "P.between(0.5, 3)", 0),
        ("1", "P.inside(0.5, 3)", 1),
        ("0.5", "P.inside(0.5, 3)", 0),
        ("1", "P.outside(1.5, 3)", 1),
        ("2", "P.outside(1.5, 3)", 0),
        ("inf", "P.gt(1)", 1),
        ("-inf", "P.lt(1)", 1),
    ];
    for (value, predicate, expected) in cases {
        assert_eq!(kept(value, predicate), expected, "{value} {predicate}");
    }
}

#[test]
fn nan_is_incomparable_except_for_neq() {
    for predicate in [
        "P.eq(nan)",
        "P.lt(nan)",
        "P.lte(nan)",
        "P.gt(nan)",
        "P.gte(nan)",
    ] {
        assert_eq!(kept("nan", predicate), 0, "nan {predicate}");
        assert_eq!(kept("1.0", predicate), 0, "1.0 {predicate}");
        assert_eq!(kept("1", predicate), 0, "1 {predicate}");
    }
    assert_eq!(kept("nan", "P.neq(nan)"), 1);
    assert_eq!(kept("1.0", "P.neq(nan)"), 1);
    for predicate in [
        "P.eq(1.0)",
        "P.lt(1.0)",
        "P.lte(1.0)",
        "P.gt(1.0)",
        "P.gte(1.0)",
        "P.between(0, 2)",
    ] {
        assert_eq!(kept("nan", predicate), 0, "nan {predicate}");
    }
    assert_eq!(kept("nan", "P.neq(1.0)"), 1);
    assert_eq!(kept("nan", "P.within([nan])"), 0);
    assert_eq!(kept("nan", "P.without([nan])"), 1);
}

#[test]
fn different_kinds_are_incomparable() {
    let cases = [
        ("1.0", r#"P.gt("foo")"#, 0),
        ("1.0", r#"P.gte("foo")"#, 0),
        ("1.0", r#"P.lt("foo")"#, 0),
        ("1.0", r#"P.lte("foo")"#, 0),
        ("1.0", r#"P.eq("foo")"#, 0),
        ("1.0", r#"P.neq("foo")"#, 1),
        (r#""foo""#, "P.gt(1.0)", 0),
        (r#""foo""#, "P.gte(1.0)", 0),
        (r#""foo""#, "P.lt(1.0)", 0),
        (r#""foo""#, "P.lte(1.0)", 0),
        (r#""27""#, "P.gt(26)", 0),
        (r#""27""#, "P.eq(27)", 0),
        (r#""foo""#, "P.between(0, 5)", 0),
        (r#""foo""#, "P.outside(0, 5)", 0),
        (r#""foo""#, "P.within([1, 2])", 0),
        ("true", "P.gt(0)", 0),
        ("true", "P.eq(1)", 0),
        ("1", "P.gt(true)", 0),
        ("()", "P.gt(0)", 0),
        ("()", "P.lte(0)", 0),
        ("0", "P.lte(())", 0),
        ("0", "P.gte(())", 0),
        ("[1, 2]", "P.gt(0)", 0),
    ];
    for (value, predicate, expected) in cases {
        assert_eq!(kept(value, predicate), expected, "{value} {predicate}");
    }
}

#[test]
fn strings_booleans_uuids_and_null_compare_with_their_own_kind() {
    let a = r#"UUID.fromString("00000000-0000-0000-0000-000000000001")"#;
    let b = r#"UUID.fromString("00000000-0000-0000-0000-000000000002")"#;
    let cases = [
        (r#""abc""#.to_string(), r#"P.lt("m")"#.to_string(), 1),
        (r#""xyz""#.to_string(), r#"P.lt("m")"#.to_string(), 0),
        (r#""m""#.to_string(), r#"P.lte("m")"#.to_string(), 1),
        (r#""xyz""#.to_string(), r#"P.gt("m")"#.to_string(), 1),
        (r#""m""#.to_string(), r#"P.gte("m")"#.to_string(), 1),
        (
            r#""m""#.to_string(),
            r#"P.between("a", "z")"#.to_string(),
            1,
        ),
        ("false".to_string(), "P.lt(true)".to_string(), 1),
        ("true".to_string(), "P.lt(false)".to_string(), 0),
        ("true".to_string(), "P.gte(true)".to_string(), 1),
        ("true".to_string(), "P.gt(false)".to_string(), 1),
        (a.to_string(), format!("P.lt({b})"), 1),
        (b.to_string(), format!("P.lt({a})"), 0),
        (b.to_string(), format!("P.gt({a})"), 1),
        (a.to_string(), format!("P.eq({a})"), 1),
        (
            a.to_string(),
            r#"P.eq("00000000-0000-0000-0000-000000000001")"#.to_string(),
            0,
        ),
        ("()".to_string(), "P.eq(())".to_string(), 1),
        ("()".to_string(), "P.lte(())".to_string(), 1),
        ("()".to_string(), "P.gte(())".to_string(), 1),
        ("()".to_string(), "P.lt(())".to_string(), 0),
        ("()".to_string(), "P.neq(())".to_string(), 0),
        ("()".to_string(), "P.neq(1)".to_string(), 1),
        ("1".to_string(), "P.neq(())".to_string(), 1),
    ];
    for (value, predicate, expected) in cases {
        assert_eq!(kept(&value, &predicate), expected, "{value} {predicate}");
    }
}

#[test]
fn property_predicates_on_the_modern_graph() {
    let run = |script: &str| eval_value(modern(), script).unwrap().as_int().unwrap();
    // `name` is a string: `gt(27)` is incomparable, so `not(...)` keeps every vertex.
    assert_eq!(run(r#"g.V().has("name", P.gt(27)).count().next()"#), 0);
    assert_eq!(
        run(r#"g.V().not(__.has("name", P.gt(27))).count().next()"#),
        6
    );
    // Ages 29, 27, 32, 35: unchanged.
    assert_eq!(run(r#"g.V().has("age", P.gt(27)).count().next()"#), 3);
    assert_eq!(run(r#"g.V().has("age", P.gte(27)).count().next()"#), 4);
    assert_eq!(run(r#"g.V().has("age", P.lt(30)).count().next()"#), 2);
    assert_eq!(
        run(r#"g.V().has("age", P.between(27.5, 33)).count().next()"#),
        2
    );
    assert_eq!(
        run(r#"g.V().has("age", P.within([27.0, 35])).count().next()"#),
        2
    );
    assert_eq!(run(r#"g.V().has("name", P.lt("m")).count().next()"#), 2);
    assert_eq!(
        run(r#"g.V().values("age").is(P.gt("x")).count().next()"#),
        0
    );
}

#[test]
fn rust_api_uses_the_same_rules() {
    let graph = GraphSource::tinkerpop_modern();
    let locked = graph.read();
    let count = |p: P<PValue>| {
        locked
            .traversal()
            .v(None)
            .values("age")
            .is(p)
            .to_list()
            .unwrap()
            .len()
    };
    assert_eq!(count(P::Eq(PValue::from(27.0))), 1);
    assert_eq!(count(P::Gt(PValue::from("x"))), 0);
    assert_eq!(count(P::Lt(PValue::from("x"))), 0);
    assert_eq!(count(P::Lt(PValue::from(f64::NAN))), 0);
    assert_eq!(count(P::Neq(PValue::from(f64::NAN))), 4);
    assert_eq!(count(P::Between(PValue::from(27.5), PValue::from(33))), 2);
}

/// The error of `script` on the modern graph.
fn error(script: &str) -> String {
    match eval_value(modern(), script) {
        Ok(value) => panic!("{script} must fail, got {value:?}"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn an_operand_that_is_not_a_single_value_is_an_error() {
    // Issue #14: a list, map or traversal operand used to be compared as its text.
    let cases = [
        (
            r#"g.v().has("name", P.gt(["a"])).count().next()"#,
            "P.gt",
            "array",
        ),
        (
            r#"g.v().has("name", P.eq(#{a: 1})).count().next()"#,
            "P.eq",
            "map",
        ),
        (
            r#"g.v().has("name", P.within([["a"]])).count().next()"#,
            "P.within",
            "array",
        ),
        (
            r#"g.v().has("name", P.without("a", [1])).count().next()"#,
            "P.without",
            "array",
        ),
        (
            r#"g.v().has("age", P.between(1, [2])).count().next()"#,
            "P.between",
            "array",
        ),
        (
            r#"g.v().has("name", TextP.containing(["a"])).count().next()"#,
            "TextP.containing",
            "array",
        ),
        (
            r#"g.v().has("name", P.gt("a").or(P.not(P.lt([1])))).count().next()"#,
            "P.lt",
            "array",
        ),
        (
            r#"g.v().values("age").is(P.eq(__.constant(1))).count().next()"#,
            "P.eq",
            "GraphTraversalSource",
        ),
        (
            r#"g.v().values("age").where(P.gt([1])).count().next()"#,
            "P.gt",
            "array",
        ),
        (
            r#"g.v().has_id(P.neq(["1"])).count().next()"#,
            "P.neq",
            "array",
        ),
        (
            r#"g.v().values("age").choose(P.gt([30]), __.constant(1), __.constant(2)).to_list()"#,
            "P.gt",
            "array",
        ),
        (
            r#"g.v().values("age").choose(__.identity()).option(P.gt([30]), __.constant(1)).to_list()"#,
            "P.gt",
            "array",
        ),
    ];
    for (script, predicate, actual) in cases {
        let message = error(script);
        assert!(
            message.contains(&format!("Argument mismatch for step '{predicate}'")),
            "{script}: {message}"
        );
        assert!(
            message.contains(&format!("got {actual}")),
            "{script}: {message}"
        );
        assert!(
            message.contains("Help: A predicate compares with single values"),
            "{script}: {message}"
        );
    }
}

#[test]
fn single_value_operands_and_element_ids_keep_working() {
    let count = |script: &str| {
        eval_value(modern(), script)
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .as_int()
            .unwrap()
    };
    // has_id(P.eq(v)) / has_id(P.within([v])) take an element by its id, as has_id(v) does.
    assert_eq!(
        count(r#"let v = g.v("1").next(); g.v().has_id(P.within([v])).count().next()"#),
        1
    );
    assert_eq!(
        count(r#"let v = g.v("1").next(); g.v().has_id(P.eq(v)).count().next()"#),
        1
    );
    // A list given to has() is the whole value, not a predicate operand.
    assert_eq!(count(r#"g.v().has("name", ["marko"]).count().next()"#), 0);
    // No arguments: the empty set.
    assert_eq!(count(r#"g.v().has("name", P.within()).count().next()"#), 0);
    assert_eq!(count(r#"g.v().has("name", P.without()).count().next()"#), 6);
    assert_eq!(count(r#"g.v().has("name", P.WithOut()).count().next()"#), 6);
    // A character is a one-letter string.
    assert_eq!(count(r#"g.v().has("name", P.lt('m')).count().next()"#), 2);
    assert_eq!(
        count(r#"g.v().has("birth", P.lt(date("2000-01-01"))).count().next()"#),
        0
    );
}
