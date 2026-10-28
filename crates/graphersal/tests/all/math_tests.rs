//! Integration tests for `math()`: the exp4j equation language (TinkerPop's `MathStep`), variable
//! resolution (`_`, map keys, side effects, step labels), the `by()` ring, the Graphersal
//! nested-access extension (`_.price.total`, `a.items[0]`), its errors and `.profile()`.
//!
//! Expected values are what TinkerPop returns on the modern graph (`g.V(1)` is `g.v("1")` here);
//! `math()` always yields a double.

#[cfg(feature = "script")]
use std::sync::Arc;

use graphersal::error::MathErrorKind;
use graphersal::prelude::PropertyMap;
use graphersal::prelude::*;

fn d(value: f64) -> ElementProperty {
    ElementProperty::from(value)
}

/// Runs a DSL script on the modern graph and renders the result.
#[cfg(feature = "script")]
fn modern(script: &str) -> String {
    let value = crate::script_host::eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
        .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
    format!("{value:?}")
}

/// Runs a DSL script on the modern graph, expecting an error, and returns its text.
#[cfg(feature = "script")]
fn modern_err(script: &str) -> String {
    match crate::script_host::eval_value(Arc::new(GraphSource::tinkerpop_modern()), script) {
        Ok(value) => panic!("`{script}` should fail, got {value:?}"),
        Err(err) => err.to_string(),
    }
}

/// Asserts `actual` holds exactly `expected`, in any order.
fn assert_same_items(actual: Vec<ElementProperty>, expected: Vec<ElementProperty>) {
    let mut rest = actual.clone();
    for item in &expected {
        match rest.iter().position(|a| a == item) {
            Some(i) => {
                rest.remove(i);
            }
            None => panic!("missing {item:?} in {actual:?}"),
        }
    }
    assert!(rest.is_empty(), "unexpected {rest:?} in {actual:?}");
}

/// The `MathErrorKind` of a failed traversal.
fn math_kind(err: &TraverserError) -> MathErrorKind {
    match err.root_cause() {
        TraverserError::MathExpression { kind, .. } => kind.clone(),
        other => panic!("expected a MathExpression error, got: {other}"),
    }
}

/// A graph with one `item` vertex whose `price` property is a nested map with a list.
fn graph_with_object_property() -> Graph {
    let graph = GraphSource::empty();
    {
        let mut items = PropertyMap::default();
        items.insert("total", ElementProperty::from(10.5));
        items.insert(
            "items",
            ElementProperty::Array(vec![
                ElementProperty::from(1),
                ElementProperty::from(2),
                ElementProperty::from(4),
            ]),
        );
        items.insert("unit price", ElementProperty::from(3));
        items.insert("currency", ElementProperty::from("EUR"));
        items.insert("discount", ElementProperty::Null);
        let mut writable = graph.write();
        writable
            .traversal_mut()
            .add_v("item")
            .property("price", ElementProperty::Object(Box::new(items)))
            .next()
            .expect("the vertex is added");
    }
    graph
}

#[cfg(feature = "script")]
#[test]
fn current_value_without_by() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .values("age")
        .math("_ / 2")
        .to_list()
        .unwrap();
    assert_eq!(res, vec![d(14.5)]);
    assert_eq!(
        modern(r#"g.V("1").values("age").math("_ / 2").toList()"#),
        "[14.5]"
    );
    assert_eq!(
        modern(r#"g.v("1").values("age").math("_ / 2").to_list()"#),
        "[14.5]"
    );
}

#[cfg(feature = "script")]
#[test]
fn current_value_with_by() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .math("_ + _")
        .by("age")
        .to_list()
        .unwrap();
    assert_same_items(res, vec![d(58.0), d(54.0), d(64.0), d(70.0)]);
    assert_eq!(
        modern(r#"g.V().math("_+_").by("age").order().toList()"#),
        "[54.0, 58.0, 64.0, 70.0]"
    );
}

#[cfg(feature = "script")]
#[test]
fn results_are_always_doubles() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .values("age")
        .math("_ + 1")
        .to_list()
        .unwrap();
    assert_eq!(res, vec![d(30.0)]);
    match &res[0] {
        ElementProperty::Number(n) => assert!(n.is_f64(), "{n:?}"),
        other => panic!("not a number: {other:?}"),
    }
    assert_eq!(modern(r#"g.inject(1).math("2^3").toList()"#), "[8.0]");
    assert_eq!(modern(r#"g.inject(1).math("2^3^2").next()"#), "512.0");
    assert_eq!(modern(r#"g.inject(1).math("-2^2").next()"#), "-4.0");
    assert_eq!(modern(r#"g.inject(16).math("sqrt(_)").next()"#), "4.0");
    assert_eq!(
        modern(r#"g.inject(1).math("sin _").next()"#),
        format!("{:?}", 1f64.sin())
    );
}

#[cfg(feature = "script")]
#[test]
fn step_labels() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .as_("a")
        .out("knows")
        .as_("b")
        .math("a + b")
        .by("age")
        .to_list()
        .unwrap();
    assert_same_items(res, vec![d(61.0), d(56.0)]);
    assert_eq!(
        modern(r#"g.V().as("a").out("knows").as("b").math("a + b").by("age").order().toList()"#),
        "[56.0, 61.0]"
    );
    assert_eq!(
        modern(r#"g.v().as_("a").out("knows").as_("b").math("a + b").by("age").order().to_list()"#),
        "[56.0, 61.0]"
    );
}

#[cfg(feature = "script")]
#[test]
fn keys_of_the_current_map() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .project(vec!["a", "b"])
        .by("age")
        .by(__::out(None).count())
        .math("a / b")
        .to_list()
        .unwrap();
    assert_eq!(res, vec![d(29.0 / 3.0)]);
    assert_eq!(
        modern(
            r#"g.V("1").project("a", "b").by("age").by(__.out().count()).math("a / b").toList()"#
        ),
        format!("[{:?}]", 29.0 / 3.0)
    );
}

#[cfg(feature = "script")]
#[test]
fn side_effects() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .with_side_effect("x", 100)
        .v(None)
        .values("age")
        .math("_ + x")
        .to_list()
        .unwrap();
    assert_same_items(res, vec![d(129.0), d(127.0), d(132.0), d(135.0)]);
    assert_eq!(
        modern(r#"g.withSideEffect("x", 100).V("1").values("age").math("_ + x").toList()"#),
        "[129.0]"
    );
    assert_eq!(
        modern(r#"g.with_side_effect("x", 100).v("1").values("age").math("_ + x").to_list()"#),
        "[129.0]"
    );
}

#[test]
fn the_by_ring_follows_first_appearance() {
    // TinkerPop: b (first) takes in("created").count(), a takes age.
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .as_("a")
        .out("created")
        .as_("b")
        .math("b + a")
        .by(__::in_("created").count())
        .by("age")
        .to_list()
        .unwrap();
    assert_same_items(res, vec![d(32.0), d(33.0), d(35.0), d(38.0)]);
}

#[cfg(feature = "script")]
#[test]
fn constants_and_their_scope_override() {
    assert_eq!(
        modern(r#"g.inject(1).math("pi").next()"#),
        format!("{:?}", std::f64::consts::PI)
    );
    assert_eq!(
        modern(r#"g.inject(1).math("e * _").next()"#),
        format!("{:?}", std::f64::consts::E)
    );
    // A scope value named like a constant wins, as in exp4j.
    assert_eq!(
        modern(r#"g.withSideEffect("e", 2).inject(1).math("e + _").next()"#),
        "3.0"
    );
}

#[cfg(feature = "script")]
#[test]
fn nested_access_on_value_map_element_map_and_project() {
    assert_eq!(
        modern(r#"g.V("1").valueMap().math("_.age[0] * 2").toList()"#),
        "[58.0]"
    );
    assert_eq!(
        modern(r#"g.v("1").element_map().math("_.age + 1").to_list()"#),
        "[30.0]"
    );
    assert_eq!(
        modern(
            r#"g.V("1").project("p", "n").by(__.elementMap()).by(__.out().count()).math("p.age / n").toList()"#
        ),
        format!("[{:?}]", 29.0 / 3.0)
    );
    assert_eq!(
        modern(
            r#"g.V("1").project("p").by(__.valueMap()).math("_.p.age[-1] + _[\"p\"][\"age\"][0]").toList()"#
        ),
        "[58.0]"
    );
}

#[cfg(feature = "script")]
#[test]
fn nested_access_after_the_by_projection() {
    assert_eq!(
        modern(r#"g.V("1").math("_.age * 2").by(__.elementMap()).toList()"#),
        "[58.0]"
    );
    assert_eq!(
        modern(r#"g.V("1").as("a").math("a.age[0] - 9").by(__.valueMap()).toList()"#),
        "[20.0]"
    );
}

#[cfg(feature = "script")]
#[test]
fn nested_access_on_inject_maps_and_lists() {
    assert_eq!(
        modern(
            r#"g.inject(#{price: #{total: 10, items: [1, 2.5, 4]}}).math("_.price.total * 2 + _.price.items[1]").toList()"#
        ),
        "[22.5]"
    );
    assert_eq!(
        modern(r#"g.inject(3, 5, 7).fold().math("_[0] + _[-1]").next()"#),
        "10.0"
    );
}

#[test]
fn nested_access_on_an_object_property() {
    let graph = graph_with_object_property();
    let lock = graph.read();
    let g = || lock.traversal();
    assert_eq!(
        g().v(None)
            .values("price")
            .math("_.total * 2")
            .to_list()
            .unwrap(),
        vec![d(21.0)]
    );
    assert_eq!(
        g().v(None)
            .math("_.price.items[2] + _.price[\"unit price\"]")
            .to_list()
            .unwrap(),
        vec![d(7.0)]
    );
    assert_eq!(
        g().v(None)
            .math("_.items[-3]")
            .by("price")
            .to_list()
            .unwrap(),
        vec![d(1.0)]
    );
    assert_eq!(
        g().v(None)
            .as_("x")
            .math("x.price.total")
            .to_list()
            .unwrap(),
        vec![d(10.5)]
    );
}

#[test]
fn nested_access_errors() {
    let graph = graph_with_object_property();
    let lock = graph.read();
    let g = || lock.traversal();

    let err = g()
        .v(None)
        .math("_.price.totl")
        .to_list()
        .expect_err("missing key");
    assert!(
        matches!(math_kind(&err), MathErrorKind::BadPath { ref path, .. } if path == "_.price.totl"),
        "{err}"
    );
    assert!(
        err.root_cause().to_string().contains("total"),
        "the available keys are listed: {err}"
    );

    let err = g()
        .v(None)
        .math("_.price.items[3]")
        .to_list()
        .expect_err("index out of range");
    assert!(
        matches!(math_kind(&err), MathErrorKind::BadPath { ref path, .. } if path == "_.price.items[3]"),
        "{err}"
    );
    assert!(
        err.root_cause().to_string().contains("out of range"),
        "{err}"
    );

    let err = g()
        .v(None)
        .math("_.price.total.x")
        .to_list()
        .expect_err("a number has no keys");
    assert!(
        matches!(math_kind(&err), MathErrorKind::BadPath { .. }),
        "{err}"
    );

    let err = g()
        .v(None)
        .math("_.price.currency")
        .to_list()
        .expect_err("a string is no number");
    assert!(
        matches!(math_kind(&err), MathErrorKind::NotANumber { ref path, ref actual, .. } if path == "_.price.currency" && actual == "string"),
        "{err}"
    );

    let err = g()
        .v(None)
        .math("_.price.discount")
        .to_list()
        .expect_err("null is no number");
    assert!(
        matches!(math_kind(&err), MathErrorKind::Null { ref path, .. } if path == "_.price.discount"),
        "{err}"
    );
}

#[cfg(feature = "script")]
#[test]
fn variable_errors() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = || lock.traversal();

    let err = g()
        .v("1")
        .values("age")
        .math("_ + x")
        .to_list()
        .expect_err("x is unbound");
    assert!(
        matches!(math_kind(&err), MathErrorKind::UnknownVariable { ref name, .. } if name == "x"),
        "{err}"
    );

    // `value` is no longer the name of the current value.
    let err = g()
        .v("1")
        .values("age")
        .math("value + 1")
        .to_list()
        .expect_err("value is unbound");
    assert!(
        matches!(math_kind(&err), MathErrorKind::UnknownVariable { ref name, .. } if name == "value"),
        "{err}"
    );

    let err = g()
        .v("1")
        .values("name")
        .math("_ + 1")
        .to_list()
        .expect_err("a string is no number");
    assert!(
        matches!(math_kind(&err), MathErrorKind::NotANumber { ref path, .. } if path == "_"),
        "{err}"
    );

    let err = g()
        .v("1")
        .values("age")
        .math("_ / 0")
        .to_list()
        .expect_err("division by zero");
    assert_eq!(math_kind(&err), MathErrorKind::DivisionByZero, "{err}");
    let err = g()
        .v("1")
        .values("age")
        .math("_ % (1 - 1)")
        .to_list()
        .expect_err("modulo by zero");
    assert_eq!(math_kind(&err), MathErrorKind::DivisionByZero, "{err}");

    assert!(
        modern_err(r#"g.V("1").values("age").math("_ + x").toList()"#).contains("variable 'x'")
    );
}

#[test]
fn parse_errors_fail_before_execution() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = || lock.traversal();

    // No traverser reaches the step, and it still fails.
    let err = g()
        .v(None)
        .has("name", "nobody")
        .math("2 +* 3")
        .to_list()
        .expect_err("syntax error");
    assert_eq!(math_kind(&err), MathErrorKind::Syntax, "{err}");
    match err.root_cause() {
        TraverserError::MathExpression { column, offset, .. } => {
            assert_eq!((*offset, *column), (3, 4), "{err}");
        }
        other => panic!("{other}"),
    }

    let err = g()
        .v(None)
        .values("age")
        .math("sqr(_)")
        .to_list()
        .expect_err("unknown function");
    assert!(
        matches!(math_kind(&err), MathErrorKind::UnknownFunction { ref name, .. } if name == "sqr"),
        "{err}"
    );

    // Also inside a child traversal that never runs.
    let err = g()
        .v(None)
        .has("name", "nobody")
        .where_t(__::math("(1"))
        .to_list()
        .expect_err("syntax error in a child");
    assert_eq!(math_kind(&err), MathErrorKind::Syntax, "{err}");

    // exp4j has no implicit multiplication (TinkerPop disables it).
    let err = g()
        .inject(vec![2])
        .math("2 _")
        .to_list()
        .expect_err("implicit multiplication");
    assert_eq!(math_kind(&err), MathErrorKind::Syntax, "{err}");
}

#[cfg(feature = "script")]
#[test]
fn empty_stream() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .has("name", "nobody")
        .math("_ + 1")
        .to_list()
        .unwrap();
    assert!(res.is_empty());
    assert_eq!(
        modern(r#"g.V().has("name", "nobody").math("_ + 1").toList()"#),
        "[]"
    );
}

#[test]
fn profile_shows_the_equation_and_its_by_modulators() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let profile = lock
        .traversal()
        .v(None)
        .as_("a")
        .out("knows")
        .as_("b")
        .math("a + b")
        .by("age")
        .profile()
        .unwrap()
        .to_string();
    assert!(profile.contains(r#"math("a + b").by("#), "{profile}");
    assert!(profile.contains("age"), "{profile}");
}

/// Asserts a Rust result and the DSL results (both spellings) of the same query.
#[cfg(feature = "script")]
fn dual(
    rust: Result<Vec<ElementProperty>, Box<TraverserError>>,
    expected: Vec<ElementProperty>,
    scripts: &[&str],
    rendered: &str,
) {
    assert_eq!(rust.expect("the Rust query runs"), expected);
    for script in scripts {
        assert_eq!(modern(script), rendered, "`{script}`");
    }
}

#[cfg(feature = "script")]
#[test]
fn rust_api_and_both_dsl_spellings_agree() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = || lock.traversal();
    dual(
        g().v("1").values("age").math("2 + 2").to_list(),
        vec![d(4.0)],
        &[
            r#"g.V("1").values("age").math("2 + 2").toList()"#,
            r#"g.v("1").values("age").math("2 + 2").to_list()"#,
        ],
        "[4.0]",
    );
    dual(
        g().v("1").values("age").math("2 + 2.5").to_list(),
        vec![d(4.5)],
        &[
            r#"g.V("1").values("age").math("2 + 2.5").toList()"#,
            r#"g.v("1").values("age").math("2 + 2.5").to_list()"#,
        ],
        "[4.5]",
    );
    dual(
        g().v("1")
            .values("age")
            .where_t(__::math("2 + 2").is(4.0))
            .to_list(),
        vec![ElementProperty::from(29)],
        &[
            r#"g.V("1").values("age").where(__.math("2 + 2").is(4.0)).toList()"#,
            r#"g.v("1").values("age").where(__.math("2 + 2").is(4.0)).to_list()"#,
        ],
        "[29]",
    );
    dual(
        g().v("1").values("age").as_("A").math("A + 100").to_list(),
        vec![d(129.0)],
        &[
            r#"g.V("1").values("age").as("A").math("A + 100").toList()"#,
            r#"g.v("1").values("age").as_("A").math("A + 100").to_list()"#,
        ],
        "[129.0]",
    );
    dual(
        g().inject(vec![42.5])
            .as_("A")
            .math("ceil(A + 100.0)")
            .to_list(),
        vec![d(143.0)],
        &[
            r#"g.inject(42.5).as("A").math("ceil(A + 100.0)").toList()"#,
            r#"g.inject(42.5).as_("A").math("ceil(A + 100.0)").to_list()"#,
        ],
        "[143.0]",
    );
    dual(
        g().v("1").math("_ + 100").by("age").to_list(),
        vec![d(129.0)],
        &[
            r#"g.V("1").math("_ + 100").by("age").toList()"#,
            r#"g.v("1").math("_ + 100").by("age").to_list()"#,
        ],
        "[129.0]",
    );
    dual(
        g().v("1")
            .values("age")
            .math("_ + 100")
            .by(__::identity())
            .to_list(),
        vec![d(129.0)],
        &[
            r#"g.V("1").values("age").math("_ + 100").by(__.identity()).toList()"#,
            r#"g.v("1").values("age").math("_ + 100").by().to_list()"#,
        ],
        "[129.0]",
    );
    dual(
        g().v("6")
            .as_("A")
            .out(None)
            .math("_ + A")
            .by(__::in_(None).count())
            .by("age")
            .to_list(),
        vec![d(38.0)],
        &[
            r#"g.V("6").as("A").out().math("_ + A").by(__.in().count()).by("age").toList()"#,
            r#"g.v("6").as_("A").out().math("_ + A").by(__.in_().count()).by("age").to_list()"#,
        ],
        "[38.0]",
    );
    let res = g()
        .v(None)
        .as_("a")
        .out("created")
        .as_("b")
        .math("b + a")
        .by(__::both(None).count().math("_ + 100"))
        .by("age")
        .to_list()
        .unwrap();
    assert_same_items(res, vec![d(132.0), d(133.0), d(135.0), d(138.0)]);
    assert_eq!(
        modern(
            r#"g.V().as("a").out("created").as("b").math("b + a").by(__.both().count().math("_ + 100")).by("age").order().toList()"#
        ),
        "[132.0, 133.0, 135.0, 138.0]"
    );
    assert!(g().v("1").values("age").math("2 +-/ 2.5").next().is_err());
}
