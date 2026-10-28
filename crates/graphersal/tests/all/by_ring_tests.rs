//! Integration tests for the steps whose `by()` modulators form TinkerPop's *traversal ring*:
//! `select()`, `project()`, `path()`, `tree()` and `math()`. The `by()`s are applied round-robin,
//! one per selected key / projected key / path object / tree level / equation variable, so one
//! `by()` applies to all of them and `select("a", "b", "c").by("name").by("lang")` reads a: name,
//! b: lang, c: name. Unproductive `by()`s follow TinkerPop 3.6+: `select()`, `path()` and `math()`
//! filter the traverser, `project()` leaves the key out, `tree()` skips the level.
//!
//! Every expected result is what TinkerPop returns for the same query on the modern graph
//! (`g.V(1)` is `g.v("1")` here). Stream order is compared as a multiset, since the adjacency
//! order of this engine's modern graph differs from TinkerGraph's.

#[cfg(feature = "script")]
use std::sync::Arc;

use graphersal::prelude::*;

/// A map with `pairs`, the materialized shape of `select()`/`project()`/`tree()` results.
fn map(pairs: &[(&str, ElementProperty)]) -> ElementProperty {
    let mut element = PropertyMap::default();
    for (key, value) in pairs {
        element.insert(*key, value.clone());
    }
    ElementProperty::Object(Box::new(element))
}

fn typed_map(pairs: &[(ElementProperty, ElementProperty)]) -> ElementProperty {
    ElementProperty::from_entries(pairs.iter().cloned())
}

fn s(value: &str) -> ElementProperty {
    ElementProperty::from(value)
}

fn n(value: i64) -> ElementProperty {
    ElementProperty::from(value)
}

/// A double, the type of every `math()` result.
fn d(value: f64) -> ElementProperty {
    ElementProperty::from(value)
}

/// A path, materialized as a list of its (projected) objects.
fn list(items: &[ElementProperty]) -> ElementProperty {
    ElementProperty::Array(items.to_vec())
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

/// Asserts `err` is an `InvalidModulator` of `step` at `position`.
fn assert_invalid_modulator(err: &TraverserError, expected_step: &str, expected_position: usize) {
    match err.root_cause() {
        TraverserError::InvalidModulator { step, position, .. } => {
            assert_eq!(step, expected_step, "wrong step in: {err}");
            assert_eq!(*position, expected_position, "wrong position in: {err}");
        }
        other => panic!("expected InvalidModulator, got: {other}"),
    }
}

// ---------------------------------------------------------------------------------------------
// select()
// ---------------------------------------------------------------------------------------------

/// TinkerPop: `g.V(1).as('a').out('knows').as('b').select('a','b').by('name')` is
/// `{a: marko, b: vadas}`, `{a: marko, b: josh}`: the one `by()` applies to both keys.
#[test]
fn select_one_by_applies_to_every_key() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("knows")
        .as_("b")
        .select(vec!["a", "b"])
        .by("name")
        .to_list()
        .unwrap();
    assert_same_items(
        res,
        vec![
            map(&[("a", s("marko")), ("b", s("vadas"))]),
            map(&[("a", s("marko")), ("b", s("josh"))]),
        ],
    );
}

/// TinkerPop: `...select('a','b').by(T.id)` is `{a: 1, b: 2}`, `{a: 1, b: 4}`.
#[test]
fn select_one_by_id_applies_to_every_key() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("knows")
        .as_("b")
        .select(vec!["a", "b"])
        .by(By::Id)
        .to_list()
        .unwrap();
    assert_same_items(
        res,
        vec![
            map(&[("a", s("1")), ("b", s("2"))]),
            map(&[("a", s("1")), ("b", s("4"))]),
        ],
    );
}

/// TinkerPop: `g.V(1).as('a').out('created').as('b').in('created').as('c')
/// .select('a','b','c').by('name').by('lang')` cycles: a: name, b: lang, c: name.
#[test]
fn select_two_by_over_three_keys_cycle() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("created")
        .as_("b")
        .in_("created")
        .as_("c")
        .select(vec!["a", "b", "c"])
        .by("name")
        .by("lang")
        .to_list()
        .unwrap();
    assert_same_items(
        res,
        vec![
            map(&[("a", s("marko")), ("b", s("java")), ("c", s("marko"))]),
            map(&[("a", s("marko")), ("b", s("java")), ("c", s("josh"))]),
            map(&[("a", s("marko")), ("b", s("java")), ("c", s("peter"))]),
        ],
    );
}

/// TinkerPop: `g.V(1).as('a').out('knows').select('a').by('name')` is `marko`, `marko`; a
/// `by(__.values('name'))` runs on the selected object, not on the current one.
#[test]
fn select_single_key_applies_by_to_the_selected_object() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("knows")
        .select("a")
        .by("name")
        .to_list()
        .unwrap();
    assert_same_items(res, vec![s("marko"), s("marko")]);

    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("knows")
        .select("a")
        .by(__::values("name"))
        .to_list()
        .unwrap();
    assert_same_items(res, vec![s("marko"), s("marko")]);
}

/// TinkerPop's `SelectOneStep` keeps one `by()`, the last one: `g.V(1).as('a').select('a')
/// .by('name').by('age')` is `29`.
#[test]
fn select_single_key_uses_the_last_by() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .select("a")
        .by("name")
        .by("age")
        .to_list()
        .unwrap();
    assert_eq!(res, vec![n(29)]);
}

/// TinkerPop 3.6+ productive `by()`: `select()` filters a traverser for which any key's `by()`
/// yields nothing. `g.V().as('a').out().as('b').select('a','b').by('age')` keeps only the two
/// person-to-person pairs; `g.V().as('a').select('a').by('age')` keeps the four people.
#[test]
fn select_unproductive_by_filters_the_traverser() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .as_("a")
        .out(None)
        .as_("b")
        .select(vec!["a", "b"])
        .by("age")
        .to_list()
        .unwrap();
    assert_same_items(
        res,
        vec![
            map(&[("a", n(29)), ("b", n(27))]),
            map(&[("a", n(29)), ("b", n(32))]),
        ],
    );

    let res = lock
        .traversal()
        .v(None)
        .as_("a")
        .select("a")
        .by("age")
        .to_list()
        .unwrap();
    assert_same_items(res, vec![n(29), n(27), n(32), n(35)]);

    let count = lock
        .traversal()
        .v(None)
        .as_("a")
        .select("a")
        .by("age")
        .count()
        .next()
        .unwrap();
    assert_eq!(count.and_then(|c| c.as_i64()), Some(4));
}

/// `select()` keys also read a map's entries: the ring applies to them too.
#[test]
fn select_ring_applies_to_map_keys() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .project(vec!["x", "y"])
        .by(By::Id)
        .by(__::out("knows").count())
        .select(vec!["x", "y"])
        .by(By::Identity)
        .to_list()
        .unwrap();
    assert_eq!(res, vec![map(&[("x", s("1")), ("y", n(2))])]);
}

/// An order projects nothing: `select().by(Order::Desc)` is an `InvalidModulator`, not a
/// silent filter (TinkerPop's `select()` takes no comparator either).
#[test]
fn select_by_order_is_rejected() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("1")
        .as_("a")
        .select("a")
        .by(graphersal::prelude::Order::Desc)
        .to_list()
        .expect_err("an order is not a projection");
    assert_invalid_modulator(&err, "select", 1);
}

// ---------------------------------------------------------------------------------------------
// project()
// ---------------------------------------------------------------------------------------------

/// TinkerPop: `g.V(1).project('a','b').by('name')` is `{a: marko, b: marko}`.
#[test]
fn project_one_by_applies_to_every_key() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .project(vec!["a", "b"])
        .by("name")
        .to_list()
        .unwrap();
    assert_eq!(res, vec![map(&[("a", s("marko")), ("b", s("marko"))])]);
}

/// TinkerPop: `g.V(1).project('a','b','c').by('name').by('age')` is
/// `{a: marko, b: 29, c: marko}`.
#[test]
fn project_two_by_over_three_keys_cycle() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .project(vec!["a", "b", "c"])
        .by("name")
        .by("age")
        .to_list()
        .unwrap();
    assert_eq!(
        res,
        vec![map(&[("a", s("marko")), ("b", n(29)), ("c", s("marko"))])]
    );
}

/// TinkerPop: without `by()` every key holds the object itself; `by()`s beyond the keys are
/// never reached.
#[test]
fn project_without_by_and_with_extra_by() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .project(vec!["a", "b"])
        .select(vec!["a", "b"])
        .by(By::Id)
        .to_list()
        .unwrap();
    assert_eq!(res, vec![map(&[("a", s("1")), ("b", s("1"))])]);

    let res = lock
        .traversal()
        .v("1")
        .project("a")
        .by("name")
        .by("age")
        .to_list()
        .unwrap();
    assert_eq!(res, vec![map(&[("a", s("marko"))])]);
}

/// TinkerPop 3.6+ productive `by()`: `project()` leaves a key out whose `by()` yields nothing,
/// and keeps the traverser. `g.V().project('n','a').by('name').by('age')` gives `{n: lop}` for
/// the software vertices.
#[test]
fn project_unproductive_by_leaves_the_key_out() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .project(vec!["n", "a"])
        .by("name")
        .by("age")
        .to_list()
        .unwrap();
    assert_same_items(
        res,
        vec![
            map(&[("n", s("marko")), ("a", n(29))]),
            map(&[("n", s("vadas")), ("a", n(27))]),
            map(&[("n", s("lop"))]),
            map(&[("n", s("josh")), ("a", n(32))]),
            map(&[("n", s("ripple"))]),
            map(&[("n", s("peter")), ("a", n(35))]),
        ],
    );
}

// ---------------------------------------------------------------------------------------------
// path()
// ---------------------------------------------------------------------------------------------

/// TinkerPop: `g.V(1).out('created').in('created').path().by('name').by('lang')` cycles:
/// `[marko, java, marko]`, `[marko, java, josh]`, `[marko, java, peter]`.
#[test]
fn path_two_by_cycle_along_the_path() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .out("created")
        .in_("created")
        .path()
        .by("name")
        .by("lang")
        .to_list()
        .unwrap();
    assert_same_items(
        res,
        vec![
            list(&[s("marko"), s("java"), s("marko")]),
            list(&[s("marko"), s("java"), s("josh")]),
            list(&[s("marko"), s("java"), s("peter")]),
        ],
    );
}

/// TinkerPop 3.6+ productive `by()`: `path()` filters a traverser with an unproductive `by()`:
/// `g.V(1).out().out().path().by('name').by('lang')` is empty (josh has no `lang`).
#[test]
fn path_unproductive_by_filters_the_traverser() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .out(None)
        .out(None)
        .path()
        .by("name")
        .by("lang")
        .to_list()
        .unwrap();
    assert!(res.is_empty(), "{res:?}");
}

// ---------------------------------------------------------------------------------------------
// tree()
// ---------------------------------------------------------------------------------------------

/// TinkerPop: `g.V(1).out('created').in('created').tree().by('name').by('lang')` is
/// `{marko: {java: {marko: {}, josh: {}, peter: {}}}}`.
#[test]
fn tree_two_by_cycle_along_each_path() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .out("created")
        .in_("created")
        .tree(None::<()>)
        .by("name")
        .by("lang")
        .to_list()
        .unwrap();
    let leaf = map(&[]);
    let expected = map(&[(
        "marko",
        map(&[(
            "java",
            map(&[
                ("marko", leaf.clone()),
                ("josh", leaf.clone()),
                ("peter", leaf),
            ]),
        )]),
    )]);
    assert_eq!(res, vec![expected]);
}

/// TinkerPop 3.6+ productive `by()`: `tree()` skips the level whose `by()` yields nothing:
/// `g.V(1).repeat(out()).times(2).tree().by('age')` is `{29: {32: {}}}` (ripple and lop have
/// no `age`).
#[test]
fn tree_unproductive_by_skips_the_level() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .repeat(__::out(None))
        .times(2)
        .tree(None::<()>)
        .by("age")
        .to_list()
        .unwrap();
    assert_eq!(
        res,
        vec![typed_map(&[(n(29), typed_map(&[(n(32), map(&[]))]))])]
    );
}

// ---------------------------------------------------------------------------------------------
// math()
// ---------------------------------------------------------------------------------------------

/// TinkerPop: `g.V().as('a').out('knows').as('b').math('a + b').by('age')` is `56`, `61`.
#[test]
fn math_one_by_applies_to_every_variable() {
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
    assert_same_items(res, vec![d(56.0), d(61.0)]);
}

/// TinkerPop: `g.V(1).as('a').out('created').as('b').in('created').as('c')
/// .math('a + b + c').by('age').by(__.in('created').count())` cycles: a: age, b: in-count,
/// c: age, giving `29 + 3 + {29, 32, 35}`.
#[test]
fn math_two_by_over_three_variables_cycle() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("created")
        .as_("b")
        .in_("created")
        .as_("c")
        .math("a + b + c")
        .by("age")
        .by(__::in_("created").count())
        .to_list()
        .unwrap();
    assert_same_items(res, vec![d(61.0), d(64.0), d(67.0)]);
}

/// TinkerPop's `MathStep` keeps the variables in a `LinkedHashSet`: a variable used twice takes
/// one ring slot. `math('a + b + a').by('age').by(__.in().count())` reads a: age, b: in-count.
#[test]
fn math_repeated_variable_takes_one_ring_slot() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("knows")
        .as_("b")
        .math("a + b + a")
        .by("age")
        .by(__::in_(None).count())
        .to_list()
        .unwrap();
    assert_same_items(res, vec![d(59.0), d(59.0)]);
}

/// TinkerPop 3.6+ productive `by()`: `math()` filters a traverser with an unproductive `by()`:
/// `g.V().math('_ + 1').by('age')` is `30.0, 28.0, 33.0, 36.0`.
#[test]
fn math_unproductive_by_filters_the_traverser() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .math("_ + 1")
        .by("age")
        .to_list()
        .unwrap();
    assert_same_items(res, vec![d(30.0), d(28.0), d(33.0), d(36.0)]);
}

/// `.profile()` shows the ring step with all its `by()`s, and a `by(__...)` child of `select()`
/// runs on the selected object (a fresh traverser), so the child's path reads are not charged to
/// the incoming path.
#[test]
fn profile_shows_the_ring_and_select_labels_only() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let result = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("knows")
        .as_("b")
        .select(vec!["a", "b"])
        .by("name")
        .profile()
        .unwrap();
    let rendered = result.to_string();
    assert!(
        rendered.contains(r#"select("a", "b").by("name")"#),
        "{rendered}"
    );
    assert!(
        rendered.contains("as(\"b\") [path: labels(a,b)]"),
        "{rendered}"
    );
    assert!(!rendered.contains("[path: full]"), "{rendered}");
}

/// Rhai, both spellings: the rendered script value of each query.
#[cfg(feature = "script")]
mod rhai {
    use super::*;

    fn render(value: &crate::script_host::rhai::Dynamic) -> String {
        if let Some(map) = value.read_lock::<crate::script_host::ScriptMap>() {
            return format!("{:?}", map.as_value_map());
        }
        if value.is_array() {
            let items: Vec<String> = value
                .clone()
                .cast::<crate::script_host::rhai::Array>()
                .iter()
                .map(render)
                .collect();
            return format!("[{}]", items.join(", "));
        }
        format!("{value:?}")
    }

    fn modern(script: &str) -> String {
        let value =
            crate::script_host::eval_value(Arc::new(GraphSource::tinkerpop_modern()), script)
                .unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
        render(&value)
    }

    #[test]
    fn ring_steps_in_both_spellings() {
        let cases = [
            (
                r#"g.V("1").as("a").out("knows").as("b").select("a", "b").by("name").order().by(__.select("b")).toList()"#,
                r#"[#{"a": "marko", "b": "josh"}, #{"a": "marko", "b": "vadas"}]"#,
            ),
            (
                r#"g.v("1").as_("a").out("knows").as_("b").select(["a", "b"]).by(T.id).order().by(__.select("b")).to_list()"#,
                r#"[#{"a": "1", "b": "2"}, #{"a": "1", "b": "4"}]"#,
            ),
            (
                r#"g.V("1").as("a").out("created").as("b").in("created").as("c").select("a", "b", "c").by("name").by("lang").order().by(__.select("c")).toList()"#,
                r#"[#{"a": "marko", "b": "java", "c": "josh"}, #{"a": "marko", "b": "java", "c": "marko"}, #{"a": "marko", "b": "java", "c": "peter"}]"#,
            ),
            (
                r#"g.V("1").as("a").out("knows").select("a").by("name").toList()"#,
                r#"["marko", "marko"]"#,
            ),
            (
                r#"g.v("1").as_("a").select("a").by("name").by("age").to_list()"#,
                "[29]",
            ),
            (r#"g.V().as("a").select("a").by("age").count().next()"#, "4"),
            (
                r#"g.V("1").project("a", "b").by("name").toList()"#,
                r#"[#{"a": "marko", "b": "marko"}]"#,
            ),
            (
                r#"g.v("1").project(["a", "b", "c"]).by("name").by("age").to_list()"#,
                r#"[#{"a": "marko", "b": 29, "c": "marko"}]"#,
            ),
            (
                r#"g.V("3").project("n", "a").by("name").by("age").toList()"#,
                r#"[#{"n": "lop"}]"#,
            ),
            (
                r#"g.V("1").out("created").in("created").path().by("name").by("lang").count().next()"#,
                "3",
            ),
            (
                r#"g.v("1").out().out().path().by("name").by("lang").to_list()"#,
                "[]",
            ),
            (
                r#"g.V("1").repeat(__.out()).times(2).tree().by("age").toList()"#,
                r#"[{29: {32: {}}}]"#,
            ),
            (
                r#"g.v("1").out("created").in_("created").tree().by("name").by("lang").to_list()"#,
                r#"[#{"marko": #{"java": #{"josh": #{}, "marko": #{}, "peter": #{}}}}]"#,
            ),
            (
                r#"g.V().as("a").out("knows").as("b").math("a + b").by("age").order().toList()"#,
                "[56.0, 61.0]",
            ),
            (
                r#"g.v("1").as_("a").out("created").as_("b").in_("created").as_("c").math("a + b + c").by("age").by(__.in_("created").count()).order().to_list()"#,
                "[61.0, 64.0, 67.0]",
            ),
            (
                r#"g.V().math("_ + 1").by("age").order().toList()"#,
                "[28.0, 30.0, 33.0, 36.0]",
            ),
        ];
        for (script, expected) in cases {
            assert_eq!(modern(script), expected, "`{script}`");
        }
    }
}
