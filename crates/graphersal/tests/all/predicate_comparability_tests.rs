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
    // Zero-argument membership predicates use the scope marker internally; keep them empty.
    assert_eq!(run(r#"g.V().has("age", P.within()).count().next()"#), 0);
    assert_eq!(run(r#"g.V().has("age", P.without()).count().next()"#), 4);
    assert_eq!(
        run(r#"g.V().values("age").is(P.gt("x")).count().next()"#),
        0
    );
}

#[test]
fn container_predicate_operands_fail_with_the_predicate_name() {
    let cases = [
        r#"g.v().has("name", P.gt(["a"])).count().next()"#,
        r#"g.v().has("name", P.eq(["a"])).count().next()"#,
        r#"g.v().has("name", P.within([["a"]])).count().next()"#,
        r#"g.v().has("name", P.gt("a").or(P.lt(["z"]))).count().next()"#,
    ];

    for script in cases {
        let error = eval_value(modern(), script).expect_err(script).to_string();
        assert!(error.contains("Argument mismatch for step 'P."), "{error}");
        assert!(error.contains("got array"), "{error}");
    }
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
