//! Integration tests for a `by(__...)` child traversal, in every step that takes one. TinkerPop
//! applies such a child with `TraversalUtil.produce()`: the step uses the child's *first*
//! result, never a list of all of them (a caller wanting every result writes
//! `by(__....fold())`), and a child yielding nothing is an unproductive `by()` that each step
//! handles by its own 3.6+ rule:
//!
//! - `order()`, `path()`, `select()`, `dedup()`, `math()`: the traverser is filtered out;
//! - `group()`/`group_count()` key: the traverser is left out of the map;
//! - `group()` value: a property/token value `by()` still creates the key (`[]` when no member
//!   has the property), a value traversal without a barrier keeps the first result of the last
//!   member that produced one, and a key no member produced a value for is left out;
//! - `project()`: the key is left out; `tree()`: the level is skipped;
//! - `aggregate()`/`store()`: nothing is added.
//!
//! An error raised inside the child fails the step instead of becoming "no value".
//!
//! Every expected result is what TinkerPop returns for the same query on the modern graph
//! (`g.V(1)` is `g.v("1")` here). A multi-result child is built with `union()` so that its first
//! result does not depend on adjacency order, which differs between this engine and TinkerGraph.

#[cfg(feature = "script")]
use std::sync::Arc;

use graphersal::prelude::Order;
use graphersal::prelude::*;

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

fn list(items: &[ElementProperty]) -> ElementProperty {
    ElementProperty::Array(items.to_vec())
}

fn strings(results: Vec<ElementProperty>) -> Vec<String> {
    results
        .iter()
        .map(|value| {
            value
                .as_string()
                .unwrap_or_else(|| panic!("expected a string, got {value:?}"))
                .to_string()
        })
        .collect()
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

/// Asserts `err` is rooted in the cast error of `as_number()` on a name.
fn assert_cast_error(err: &TraverserError) {
    match err.root_cause() {
        TraverserError::Value(ValueError::CastException { .. }) => {}
        other => panic!("expected a CastException from the child traversal, got: {other}"),
    }
}

/// `union(values(first), values(second))`: a child with two results for an element that has
/// both properties, whose first result is `first`.
fn two<'g, S: GraphStorage + 'g>(first: &str, second: &str) -> AnonymousTraversal<'g, S> {
    __::union(vec![__::values(first), __::values(second)])
}

/// `union(values("lang"), constant("none"))`: `[lang, "none"]` for software, `["none"]` for
/// persons.
fn lang_or_none<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
    __::union(vec![__::values("lang"), __::constant("none")])
}

/// A child that fails on every vertex: `values("name").as_number(GType.LONG)`.
fn failing<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
    __::values("name").as_number(GType::LONG)
}

// ---------------------------------------------------------------------------------------------
// order()
// ---------------------------------------------------------------------------------------------

/// TinkerPop: `g.V().hasLabel('person').order().by(__.out().values('name').order())
/// .values('name')` is `[marko, josh, peter]`: sort keys josh, lop, lop (the first, smallest,
/// name), and vadas (no out-neighbor) is filtered out. With `.order().by(desc)` inside, the
/// first name is the largest: vadas, ripple, lop, giving `[peter, josh, marko]`.
#[test]
fn order_sorts_by_the_first_result_and_filters_the_unproductive() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let names = lock
        .traversal()
        .v(None)
        .has_label("person")
        .order()
        .by(__::out(None).values("name").order())
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(strings(names), ["marko", "josh", "peter"]);
    let names = lock
        .traversal()
        .v(None)
        .has_label("person")
        .order()
        .by(__::out(None).values("name").order().by(Order::Desc))
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(strings(names), ["peter", "josh", "marko"]);
}

/// TinkerPop 3.6+: `g.V().order().by('age').values('name')` (and `by(__.values('age'))`) is
/// `[vadas, marko, josh, peter]`: the software vertices have no `age` and are filtered out
/// rather than sorted as a null key.
#[test]
fn order_filters_a_missing_property_or_an_empty_child() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let names = lock
        .traversal()
        .v(None)
        .order()
        .by("age")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(strings(names), ["vadas", "marko", "josh", "peter"]);
    let names = lock
        .traversal()
        .v(None)
        .order()
        .by(__::values("age"))
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(strings(names), ["vadas", "marko", "josh", "peter"]);
}

// ---------------------------------------------------------------------------------------------
// project() / select() / path() / tree() / math()
// ---------------------------------------------------------------------------------------------

/// TinkerPop: `g.V().hasLabel('person').project('a').by(union(values('name'), values('age')))`
/// is `{a: marko}`, `{a: vadas}`, ...: the first result, not `[name, age]`. `g.V(2)
/// .project('n', 'o').by('name').by(out())` is `{n: vadas}`: the key of the empty child is left
/// out.
#[test]
fn project_uses_the_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .has_label("person")
        .project("a")
        .by(two("name", "age"))
        .to_list()
        .unwrap();
    let expected = ["marko", "vadas", "josh", "peter"]
        .iter()
        .map(|name| map(&[("a", s(name))]))
        .collect::<Vec<_>>();
    assert_eq!(res, expected);
    let res = lock
        .traversal()
        .v("2")
        .project(vec!["n", "o"])
        .by("name")
        .by(__::out(None))
        .to_list()
        .unwrap();
    assert_eq!(res, vec![map(&[("n", s("vadas"))])]);
}

/// The child stops at its first result, as TinkerPop's lazy `produce()` does: the second
/// `union()` branch yields `"x"`, which `as_number()` would reject, but it is never reached.
#[test]
fn a_child_stops_at_its_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .has_label("person")
        .project("a")
        .by(__::union(vec![__::values("age"), __::constant("x")]).as_number(GType::LONG))
        .to_list()
        .unwrap();
    let expected = [29, 27, 32, 35]
        .iter()
        .map(|age| map(&[("a", n(*age))]))
        .collect::<Vec<_>>();
    assert_eq!(res, expected);
}

/// TinkerPop: `g.V(1).as('a').out('knows').as('b').select('a', 'b')
/// .by(union(values('name'), values('age')))` is `{a: marko, b: vadas}`, `{a: marko, b: josh}`.
#[test]
fn select_uses_the_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .out("knows")
        .as_("b")
        .select(vec!["a", "b"])
        .by(two("name", "age"))
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

/// TinkerPop: `g.V(1).out('knows').path().by(union(values('age'), values('name')))` is
/// `[29, 27]`, `[29, 32]`; `g.V(1).out().path().by(values('age'))` drops the path through lop.
#[test]
fn path_uses_the_first_result_and_filters_the_unproductive() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .out("knows")
        .path()
        .by(two("age", "name"))
        .to_list()
        .unwrap();
    assert_same_items(res, vec![list(&[n(29), n(27)]), list(&[n(29), n(32)])]);
    let res = lock
        .traversal()
        .v("1")
        .out(None)
        .path()
        .by(__::values("age"))
        .to_list()
        .unwrap();
    assert_same_items(res, vec![list(&[n(29), n(27)]), list(&[n(29), n(32)])]);
}

/// TinkerPop: `g.V(1).out('knows').tree().by(union(values('age'), values('name')))` is
/// `{29: {27: {}, 32: {}}}`.
#[test]
fn tree_uses_the_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .out("knows")
        .tree(None::<()>)
        .by(two("age", "name"))
        .to_list()
        .unwrap();
    assert_eq!(
        res,
        vec![typed_map(&[(
            n(29),
            typed_map(&[(n(27), map(&[])), (n(32), map(&[]))])
        )])]
    );
}

/// TinkerPop: `g.V(1).as('a').math('a + 1').by(union(values('age'), constant(100)))` is `30.0`.
#[test]
fn math_uses_the_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v("1")
        .as_("a")
        .math("a + 1")
        .by(__::union(vec![__::values("age"), __::constant(100)]))
        .to_list()
        .unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].as_f64(), Some(30.0), "{res:?}");
}

// ---------------------------------------------------------------------------------------------
// dedup() / group() / group_count() / aggregate() / store()
// ---------------------------------------------------------------------------------------------

/// TinkerPop: `g.V().dedup().by(union(values('lang'), constant('none'))).values('name')` is
/// `[marko, lop]`: keys none (persons) and java (software).
#[test]
fn dedup_uses_the_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let names = lock
        .traversal()
        .v(None)
        .dedup()
        .by(lang_or_none())
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(strings(names), ["marko", "lop"]);
}

/// TinkerPop: `g.V().group().by(union(values('age'), values('name'))).by('name')` keys each
/// person by its age and each software by its name.
#[test]
fn group_key_uses_the_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .group()
        .by(two("age", "name"))
        .by("name")
        .to_list()
        .unwrap();
    let expected = typed_map(&[
        (n(29), list(&[s("marko")])),
        (n(27), list(&[s("vadas")])),
        (s("lop"), list(&[s("lop")])),
        (n(32), list(&[s("josh")])),
        (s("ripple"), list(&[s("ripple")])),
        (n(35), list(&[s("peter")])),
    ]);
    assert_eq!(res, vec![expected]);
}

/// TinkerPop: `g.V().groupCount().by(union(values('lang'), constant('none')))` is
/// `{java: 2, none: 4}`; `g.V().groupCount().by(values('lang'))` is `{java: 2}` (the persons'
/// key is unproductive), and `g.V().group().by(values('age')).by('name')` holds only persons.
#[test]
fn group_count_key_uses_the_first_result_and_skips_the_unproductive() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .group_count()
        .by(lang_or_none())
        .to_list()
        .unwrap();
    assert_eq!(res, vec![map(&[("java", n(2)), ("none", n(4))])]);
    let res = lock
        .traversal()
        .v(None)
        .group_count()
        .by(__::values("lang"))
        .to_list()
        .unwrap();
    assert_eq!(res, vec![map(&[("java", n(2))])]);
    let res = lock
        .traversal()
        .v(None)
        .group()
        .by(__::values("age"))
        .by("name")
        .to_list()
        .unwrap();
    let expected = typed_map(&[
        (n(29), list(&[s("marko")])),
        (n(27), list(&[s("vadas")])),
        (n(32), list(&[s("josh")])),
        (n(35), list(&[s("peter")])),
    ]);
    assert_eq!(res, vec![expected]);
}

/// TinkerPop's `GroupStep` runs a value traversal without a barrier per member and keeps, per
/// key, the first result of the last member that produced one (`Operator.assign`):
/// `g.V().group().by(label).by(union(values('name'), values('age')))` is
/// `{person: peter, software: ripple}`, and `by(out().values('name'))` has no `software` key
/// (no software vertex has an out-neighbor).
#[test]
fn group_value_traversal_without_a_barrier_keeps_the_last_members_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let res = lock
        .traversal()
        .v(None)
        .group()
        .by(By::Label)
        .by(two("name", "age"))
        .to_list()
        .unwrap();
    assert_eq!(
        res,
        vec![map(&[("person", s("peter")), ("software", s("ripple"))])]
    );
    let res = lock
        .traversal()
        .v(None)
        .group()
        .by(By::Label)
        .by(__::out(None).values("name"))
        .to_list()
        .unwrap();
    match &res[..] {
        [ElementProperty::Object(groups)] => {
            assert!(
                groups
                    .get("person")
                    .is_some_and(|v| v.as_string().is_some()),
                "{res:?}"
            );
            assert!(groups.get("software").is_none(), "{res:?}");
        }
        other => panic!("expected one map, got {other:?}"),
    }
}

/// TinkerPop runs a property value `by()` as `map(values(key)).fold()`, so the key of a group
/// none of whose members has the property still exists, with `[]`:
/// `g.V().group().by(label).by('age')` is `{person: [29, 27, 32, 35], software: []}`.
#[test]
fn group_value_by_keeps_a_productive_key_with_an_empty_list() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let expected = map(&[
        ("person", list(&[n(29), n(27), n(32), n(35)])),
        ("software", list(&[])),
    ]);
    let res = lock
        .traversal()
        .v(None)
        .group()
        .by(By::Label)
        .by("age")
        .to_list()
        .unwrap();
    assert_eq!(res, vec![expected]);
}

/// TinkerPop: `g.V().aggregate('x').by(union(values('lang'), constant('none'))).cap('x')` holds
/// none four times and java twice; `store('x').by(values('age'))` holds only the four ages.
#[test]
fn aggregate_and_store_use_the_first_result() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let expected = vec![
        s("none"),
        s("none"),
        s("java"),
        s("none"),
        s("java"),
        s("none"),
    ];
    let res = lock
        .traversal()
        .v(None)
        .aggregate("x")
        .by(lang_or_none())
        .cap("x")
        .to_list()
        .unwrap();
    match &res[..] {
        [ElementProperty::Array(items)] => assert_same_items(items.clone(), expected.clone()),
        other => panic!("expected one list, got {other:?}"),
    }
    let res = lock
        .traversal()
        .v(None)
        .store("x")
        .by(lang_or_none())
        .cap("x")
        .to_list()
        .unwrap();
    match &res[..] {
        [ElementProperty::Array(items)] => assert_same_items(items.clone(), expected),
        other => panic!("expected one list, got {other:?}"),
    }
    let res = lock
        .traversal()
        .v(None)
        .store("x")
        .by(__::values("age"))
        .cap("x")
        .to_list()
        .unwrap();
    match &res[..] {
        [ElementProperty::Array(items)] => {
            assert_same_items(items.clone(), vec![n(29), n(27), n(32), n(35)])
        }
        other => panic!("expected one list, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Errors inside the child
// ---------------------------------------------------------------------------------------------

/// A child that fails (`as_number()` on a name) fails the step in every step taking a
/// `by(__...)`: the error is no longer swallowed as an unproductive `by()`.
#[test]
fn an_error_inside_the_child_propagates() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = || lock.traversal();
    let results: Vec<(&str, TraverserResult<Vec<ElementProperty>>)> = vec![
        ("order", g().v(None).order().by(failing()).to_list()),
        ("project", g().v(None).project("a").by(failing()).to_list()),
        (
            "select",
            g().v(None)
                .as_("a")
                .select(vec!["a", "a"])
                .by(failing())
                .to_list(),
        ),
        (
            "select one",
            g().v(None).as_("a").select("a").by(failing()).to_list(),
        ),
        ("path", g().v(None).path().by(failing()).to_list()),
        ("tree", g().v(None).tree(None::<()>).by(failing()).to_list()),
        (
            "math",
            g().v(None).as_("a").math("a + 1").by(failing()).to_list(),
        ),
        ("dedup", g().v(None).dedup().by(failing()).to_list()),
        ("group key", g().v(None).group().by(failing()).to_list()),
        (
            "group value",
            g().v(None).group().by(By::Label).by(failing()).to_list(),
        ),
        (
            "group value fold",
            g().v(None)
                .group()
                .by(By::Label)
                .by(failing().fold())
                .to_list(),
        ),
        (
            "group_count",
            g().v(None).group_count().by(failing()).to_list(),
        ),
        (
            "aggregate",
            g().v(None).aggregate("x").by(failing()).cap("x").to_list(),
        ),
        (
            "store",
            g().v(None).store("x").by(failing()).cap("x").to_list(),
        ),
        (
            "value_map",
            g().v(None)
                .value_map(None::<()>)
                .by(__::unfold().as_number(GType::LONG))
                .to_list(),
        ),
    ];
    for (step, result) in results {
        let err = result.expect_err(step);
        assert_cast_error(&err);
    }
}

#[cfg(feature = "script")]
mod rhai {
    use super::*;

    /// Evaluates a Rhai script ending on a terminal and renders its value.
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

    /// Runs a Rhai query that ends on a traversal and returns its materialized results.
    fn run(script: &str) -> TraverserResult<Vec<ElementProperty>> {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
        let lock = graph.read();
        gts.ast_convert_to_gts(&*lock).to_list()
    }

    #[test]
    fn first_result_and_productive_by_in_both_spellings() {
        let cases = [
            (
                r#"g.V().hasLabel("person").order().by(__.out().values("name").order()).values("name").toList()"#,
                r#"["marko", "josh", "peter"]"#,
            ),
            (
                r#"g.v().has_label("person").order().by(__.out().values("name").order().by(Order.desc)).values("name").to_list()"#,
                r#"["peter", "josh", "marko"]"#,
            ),
            (
                r#"g.V().order().by(__.values("age")).values("name").toList()"#,
                r#"["vadas", "marko", "josh", "peter"]"#,
            ),
            (
                r#"g.v().order().by("age").values("name").to_list()"#,
                r#"["vadas", "marko", "josh", "peter"]"#,
            ),
            (
                r#"g.V().hasLabel("person").project("a").by(__.union(__.values("name"), __.values("age"))).toList()"#,
                r#"[#{"a": "marko"}, #{"a": "vadas"}, #{"a": "josh"}, #{"a": "peter"}]"#,
            ),
            (
                r#"g.v().has_label("person").project("a").by(__.union(__.values("age"), __.constant("x")).as_number(GType.LONG)).to_list()"#,
                r#"[#{"a": 29}, #{"a": 27}, #{"a": 32}, #{"a": 35}]"#,
            ),
            (
                r#"g.V("2").project("n", "o").by("name").by(__.out()).toList()"#,
                r#"[#{"n": "vadas"}]"#,
            ),
            (
                r#"g.V("1").as("a").out("knows").as("b").select("a", "b").by(__.union(__.values("name"), __.values("age"))).order().by(__.select("b")).toList()"#,
                r#"[#{"a": "marko", "b": "josh"}, #{"a": "marko", "b": "vadas"}]"#,
            ),
            (
                r#"g.v("1").out("knows").has("name", "vadas").path().by(__.union(__.values("age"), __.values("name"))).to_list()"#,
                "[[29, 27]]",
            ),
            (
                r#"g.V("1").out("knows").tree().by(__.union(__.values("age"), __.values("name"))).toList()"#,
                r#"[{29: {32: {}, 27: {}}}]"#,
            ),
            // `math()` always yields a double, as TinkerPop's does.
            (
                r#"g.V("1").as("a").math("a + 1").by(__.union(__.values("age"), __.constant(100))).is(30.0).count().next()"#,
                "1",
            ),
            (
                r#"g.V().dedup().by(__.union(__.values("lang"), __.constant("none"))).values("name").toList()"#,
                r#"["marko", "lop"]"#,
            ),
            (
                r#"g.V().group().by(__.union(__.values("age"), __.values("name"))).by("name").next()"#,
                r#"{29: ["marko"], 27: ["vadas"], "lop": ["lop"], 32: ["josh"], "ripple": ["ripple"], 35: ["peter"]}"#,
            ),
            (
                r#"g.v().group_count().by(__.union(__.values("lang"), __.constant("none"))).next()"#,
                r#"#{"java": 2, "none": 4}"#,
            ),
            (
                r#"g.V().groupCount().by(__.values("lang")).next()"#,
                r#"#{"java": 2}"#,
            ),
            (
                r#"g.V().group().by(T.label).by(__.union(__.values("name"), __.values("age"))).next()"#,
                r#"#{"person": "peter", "software": "ripple"}"#,
            ),
            (
                r#"g.v().group().by(T::label).by("age").next()"#,
                r#"#{"person": [29, 27, 32, 35], "software": []}"#,
            ),
            (
                r#"let xs = g.V().aggregate("x").by(__.union(__.values("lang"), __.constant("none"))).cap("x").next(); xs.sort(); xs"#,
                r#"["java", "java", "none", "none", "none", "none"]"#,
            ),
            (
                r#"let xs = g.v().store("x").by(__.values("age")).cap("x").next(); xs.sort(); xs"#,
                "[27, 29, 32, 35]",
            ),
        ];
        for (script, expected) in cases {
            assert_eq!(modern(script), expected, "`{script}`");
        }
    }

    #[test]
    fn an_error_inside_the_child_propagates() {
        let scripts = [
            r#"g.V().order().by(__.values("name").as_number(GType.LONG))"#,
            r#"g.v().project("a").by(__.values("name").as_number(GType.LONG))"#,
            r#"g.V().as("a").select("a", "a").by(__.values("name").as_number(GType.LONG))"#,
            r#"g.V().path().by(__.values("name").as_number(GType.LONG))"#,
            r#"g.v().tree().by(__.values("name").as_number(GType.LONG))"#,
            r#"g.V().as("a").math("a + 1").by(__.values("name").as_number(GType.LONG))"#,
            r#"g.V().dedup().by(__.values("name").as_number(GType.LONG))"#,
            r#"g.V().group().by(__.values("name").as_number(GType.LONG))"#,
            r#"g.v().group().by(T.label).by(__.values("name").as_number(GType.LONG))"#,
            r#"g.V().groupCount().by(__.values("name").as_number(GType.LONG))"#,
            r#"g.V().aggregate("x").by(__.values("name").as_number(GType.LONG)).cap("x")"#,
            r#"g.v().store("x").by(__.values("name").as_number(GType.LONG)).cap("x")"#,
            r#"g.V().valueMap().by(__.unfold().as_number(GType.LONG))"#,
        ];
        for script in scripts {
            let err = run(script).expect_err(script);
            assert_cast_error(&err);
        }
    }
}
