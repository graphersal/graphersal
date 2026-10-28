//! Integration tests for the `Scope` token and the `Scope::Local` steps (TinkerPop
//! `count(local)`, `sum(local)`, ...), in the Rust API and in the Rhai DSL.
//!
//! Every behaviour is checked through `<step>_scoped(Scope::Local)` in Rust and through both DSL
//! spellings `Scope.local` and `Scope::local`.

use graphersal::prelude::Order;
use graphersal::prelude::*;

/// A traversal result, the label its check is reported under, and the expected strings.
type RenderCase<'a> = (
    TraverserResult<Vec<ElementProperty>>,
    &'a str,
    &'a [&'a str],
);

/// An expected scalar result, comparable against both a Rust `ElementProperty` and a Rhai value.
#[derive(Debug, Clone, Copy)]
enum Exp {
    /// A long.
    I(i64),
    /// A double.
    F(f64),
}

impl Exp {
    fn matches_rust(self, value: &ElementProperty) -> bool {
        match self {
            Exp::I(i) => value == &ElementProperty::Number(i.into()),
            Exp::F(f) => value == &ElementProperty::Number(f.into()),
        }
    }
}

/// The modern graph plus one vertex holding a list property and one holding a map property,
/// as `data/List.feature` / `data/Map.feature` build them.
fn collections_graph() -> Graph {
    let graph = GraphSource::empty();
    {
        let mut lock = graph.write();
        let list: Vec<ElementProperty> = (1..=5_i64).map(ElementProperty::from).collect();
        lock.traversal_mut()
            .add_v("data")
            .property("list", ElementProperty::Array(list))
            .next()
            .expect("add list vertex");
    }
    {
        let mut map = graphersal::prelude::PropertyMap::default();
        for (key, value) in [("a", 1_i64), ("b", 2), ("c", 3)] {
            map.insert(key, value.into());
        }
        let mut lock = graph.write();
        lock.traversal_mut()
            .add_v("data")
            .property("map", ElementProperty::Object(Box::new(map)))
            .next()
            .expect("add map vertex");
    }
    graph
}

#[cfg(feature = "script")]
const COLLECTIONS_SETUP: &str = r#"g.addV("data").property("list", [1, 2, 3, 4, 5]).next();
g.addV("data").property("map", #{"a": 1, "b": 2, "c": 3}).next();
"#;

#[cfg(feature = "script")]
mod dsl {
    use super::Exp;
    use crate::script_host::eval_value;
    use graphersal::prelude::*;
    use std::sync::Arc;

    /// Evaluates `script` (which must end in a list terminal) against `graph`.
    pub(super) fn list(graph: Graph, script: &str) -> Vec<crate::script_host::rhai::Dynamic> {
        eval_value(Arc::new(graph), script)
            .unwrap_or_else(|err| panic!("{script}: {err}"))
            .into_array()
            .unwrap_or_else(|err| panic!("{script}: not a list ({err})"))
    }

    /// Asserts that `script` (evaluated once with `Scope.local` and once with `Scope::local`)
    /// yields `expected`, in any order.
    pub(super) fn assert_both(graph: fn() -> Graph, setup: &str, script: &str, expected: &[Exp]) {
        for spelling in ["Scope.local", "Scope::local"] {
            let query = format!("{setup}{}", script.replace("Scope.local", spelling));
            let values = list(graph(), &query);
            assert_eq!(values.len(), expected.len(), "{query}: {values:?}");
            let mut pending: Vec<Exp> = expected.to_vec();
            for value in &values {
                let position = pending.iter().position(|exp| match exp {
                    Exp::I(i) => value.as_int().ok() == Some(*i),
                    Exp::F(f) => value.as_float().ok() == Some(*f),
                });
                match position {
                    Some(position) => {
                        pending.remove(position);
                    }
                    None => panic!("{query}: unexpected {value:?} (expected {expected:?})"),
                }
            }
        }
    }
}

/// Asserts a Rust result against `expected`, in any order.
fn assert_rust(values: TraverserResult<Vec<ElementProperty>>, expected: &[Exp]) {
    let values = values.expect("traversal");
    assert_eq!(values.len(), expected.len(), "{values:?}");
    let mut pending = expected.to_vec();
    for value in &values {
        match pending.iter().position(|exp| exp.matches_rust(value)) {
            Some(position) => {
                pending.remove(position);
            }
            None => panic!("unexpected {value:?} (expected {expected:?})"),
        }
    }
}

fn modern() -> Graph {
    GraphSource::tinkerpop_modern()
}

fn empty() -> Graph {
    GraphSource::empty()
}

// ---------------------------------------------------------------------------------------------
// count(local)
// ---------------------------------------------------------------------------------------------

#[test]
fn count_local_of_a_folded_list() {
    let graph = modern();
    let lock = graph.read();
    assert_rust(
        lock.traversal()
            .v(None)
            .fold()
            .count_scoped(Scope::Local)
            .to_list(),
        &[Exp::I(6)],
    );
    #[cfg(feature = "script")]
    dsl::assert_both(
        modern,
        "",
        r#"g.V().fold().count(Scope.local).toList()"#,
        &[Exp::I(6)],
    );
}

#[test]
fn count_local_of_a_map() {
    let graph = modern();
    let lock = graph.read();
    assert_rust(
        lock.traversal()
            .v(None)
            .group()
            .by(By::Label)
            .count_scoped(Scope::Local)
            .to_list(),
        &[Exp::I(2)],
    );
    #[cfg(feature = "script")]
    dsl::assert_both(
        modern,
        "",
        r#"g.V().group().by(T.label).count(Scope.local).toList()"#,
        &[Exp::I(2)],
    );
}

#[test]
fn count_local_of_a_scalar_is_one_and_is_not_pushed_down() {
    let graph = modern();
    let lock = graph.read();
    assert_rust(
        lock.traversal()
            .v(None)
            .count_scoped(Scope::Local)
            .to_list(),
        &[Exp::I(1); 6],
    );
    #[cfg(feature = "script")]
    dsl::assert_both(
        modern,
        "",
        r#"g.V().count(Scope.local).toList()"#,
        &[Exp::I(1); 6],
    );

    let profile = lock
        .traversal()
        .v(None)
        .count_scoped(Scope::Local)
        .profile()
        .unwrap()
        .to_string();
    assert!(profile.contains("count(Scope.local)"), "{profile}");
    assert!(!profile.contains("count_pushdown"), "{profile}");
}

#[test]
fn count_local_profile_shows_the_local_step() {
    let graph = modern();
    let lock = graph.read();
    let profile = lock
        .traversal()
        .v(None)
        .fold()
        .count_scoped(Scope::Local)
        .profile()
        .unwrap()
        .to_string();
    assert!(profile.contains("count(Scope.local)"), "{profile}");
}

#[test]
fn count_local_of_a_lazy_list_and_map_property() {
    let graph = collections_graph();
    let lock = graph.read();
    assert_rust(
        lock.traversal()
            .v(None)
            .values("list")
            .is(P::type_of(GType::LIST))
            .count_scoped(Scope::Local)
            .to_list(),
        &[Exp::I(5)],
    );
    assert_rust(
        lock.traversal()
            .v(None)
            .values("map")
            .is(P::type_of(GType::MAP))
            .count_scoped(Scope::Local)
            .to_list(),
        &[Exp::I(3)],
    );
    #[cfg(feature = "script")]
    {
        dsl::assert_both(
            empty,
            COLLECTIONS_SETUP,
            r#"g.V().values("list").is(P.typeOf(GType.LIST)).count(Scope.local).toList()"#,
            &[Exp::I(5)],
        );
        dsl::assert_both(
            empty,
            COLLECTIONS_SETUP,
            r#"g.V().values("map").is(P.typeOf(GType.MAP)).count(Scope.local).toList()"#,
            &[Exp::I(3)],
        );
    }
}

#[cfg(feature = "script")]
#[test]
fn count_local_of_a_tree_side_effect() {
    // The tree of `out()` has three roots (marko, josh, peter).
    dsl::assert_both(
        modern,
        "",
        r#"g.V().out().tree("a").cap("a").count(Scope.local).toList()"#,
        &[Exp::I(3)],
    );
}

#[test]
fn count_local_nested_in_where_and_project() {
    let graph = modern();
    let lock = graph.read();
    // Vertices with more than two outgoing neighbours: only marko.
    let names = lock
        .traversal()
        .v(None)
        .where_t(__::out(None).fold().count_scoped(Scope::Local).is(P::Gt(2)))
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names, vec![ElementProperty::String("marko".into())]);
    let counts = lock
        .traversal()
        .v("1")
        .project(vec!["n"])
        .by(__::out(None).fold().count_scoped(Scope::Local))
        .select(Column::Values)
        .unfold()
        .to_list();
    assert_rust(counts, &[Exp::I(3)]);
    #[cfg(feature = "script")]
    {
        dsl::assert_both(
            modern,
            "",
            r#"g.V().where(__.out().fold().count(Scope.local).is(P.gt(2))).count().toList()"#,
            &[Exp::I(1)],
        );
        dsl::assert_both(
            modern,
            "",
            r#"g.V("1").project("n").by(__.out().fold().count(Scope.local)).select(Column.values).unfold().toList()"#,
            &[Exp::I(3)],
        );
    }
}

// ---------------------------------------------------------------------------------------------
// sum/min/max/mean(local)
// ---------------------------------------------------------------------------------------------

#[test]
fn reducers_on_a_folded_list() {
    let graph = modern();
    let lock = graph.read();
    let g = || lock.traversal().v(None).values("age").fold();
    assert_rust(g().sum_scoped(Scope::Local).to_list(), &[Exp::I(123)]);
    assert_rust(g().min_scoped(Scope::Local).to_list(), &[Exp::I(27)]);
    assert_rust(g().max_scoped(Scope::Local).to_list(), &[Exp::I(35)]);
    assert_rust(g().mean_scoped(Scope::Local).to_list(), &[Exp::F(30.75)]);
    #[cfg(feature = "script")]
    for (step, expected) in [
        ("sum", Exp::I(123)),
        ("min", Exp::I(27)),
        ("max", Exp::I(35)),
        ("mean", Exp::F(30.75)),
    ] {
        dsl::assert_both(
            modern,
            "",
            &format!(r#"g.V().values("age").fold().{step}(Scope.local).toList()"#),
            &[expected],
        );
    }
}

#[test]
fn reducers_on_an_empty_list_emit_nothing() {
    let graph = modern();
    let lock = graph.read();
    let g = || lock.traversal().v(None).values("foo").fold();
    assert_rust(g().sum_scoped(Scope::Local).to_list(), &[]);
    assert_rust(g().min_scoped(Scope::Local).to_list(), &[]);
    assert_rust(g().max_scoped(Scope::Local).to_list(), &[]);
    assert_rust(g().mean_scoped(Scope::Local).to_list(), &[]);
    #[cfg(feature = "script")]
    for step in ["sum", "min", "max", "mean"] {
        dsl::assert_both(
            modern,
            "",
            &format!(r#"g.V().values("foo").fold().{step}(Scope.local).toList()"#),
            &[],
        );
    }
}

/// The local reducers may drop a traverser, so a fused `count()` after them must still run them.
#[test]
fn fused_count_honours_a_dropping_reducer() {
    let graph = modern();
    let lock = graph.read();
    assert_rust(
        lock.traversal()
            .v(None)
            .values("foo")
            .fold()
            .sum_scoped(Scope::Local)
            .count()
            .to_list(),
        &[Exp::I(0)],
    );
    #[cfg(feature = "script")]
    dsl::assert_both(
        modern,
        "",
        r#"g.V().values("foo").fold().sum(Scope.local).count().toList()"#,
        &[Exp::I(0)],
    );
}

#[test]
fn reducers_on_a_scalar_reduce_the_value_itself() {
    let graph = modern();
    let lock = graph.read();
    let g = || lock.traversal().v("1").values("age");
    assert_rust(g().sum_scoped(Scope::Local).to_list(), &[Exp::I(29)]);
    assert_rust(g().min_scoped(Scope::Local).to_list(), &[Exp::I(29)]);
    assert_rust(g().max_scoped(Scope::Local).to_list(), &[Exp::I(29)]);
    assert_rust(g().mean_scoped(Scope::Local).to_list(), &[Exp::F(29.0)]);
    #[cfg(feature = "script")]
    for (step, expected) in [
        ("sum", Exp::I(29)),
        ("min", Exp::I(29)),
        ("max", Exp::I(29)),
        ("mean", Exp::F(29.0)),
    ] {
        dsl::assert_both(
            modern,
            "",
            &format!(r#"g.V("1").values("age").{step}(Scope.local).toList()"#),
            &[expected],
        );
    }
}

#[test]
fn reducers_on_a_captured_aggregate() {
    let graph = modern();
    let lock = graph.read();
    assert_rust(
        lock.traversal()
            .v(None)
            .aggregate("a")
            .by("age")
            .cap(vec!["a"])
            .sum_scoped(Scope::Local)
            .to_list(),
        &[Exp::I(123)],
    );
    #[cfg(feature = "script")]
    dsl::assert_both(
        modern,
        "",
        r#"g.V().aggregate("a").by("age").cap("a").sum(Scope.local).toList()"#,
        &[Exp::I(123)],
    );
}

/// `mean(local)` works in the floating-point domain; `sum/min/max(local)` promote a long + double
/// mix to a double, like their global counterparts.
#[test]
fn local_reducers_mix_numeric_domains() {
    let graph = modern();
    let lock = graph.read();
    let mixed = || {
        lock.traversal()
            .v(None)
            .local(__::union(vec![__::values("age"), __::out_e(None).values("weight")]).fold())
    };
    assert_rust(
        mixed().mean_scoped(Scope::Local).to_list(),
        &[
            Exp::F(7.725),
            Exp::F(27.0),
            Exp::F(11.133333333333333),
            Exp::F(17.6),
        ],
    );
    // `sum/min/max(local)` promote an int + float mix to a float.
    let sums = mixed().sum_scoped(Scope::Local).to_list().expect("sum");
    let mut got: Vec<f64> = sums
        .iter()
        .filter_map(|v| v.as_f64().or(v.as_i64().map(|i| i as f64)))
        .collect();
    got.sort_by(|a, b| a.total_cmp(b));
    let want = [27.0, 30.9, 33.4, 35.2];
    assert_eq!(got.len(), 4, "{sums:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-9, "{sums:?}");
    }
    #[cfg(feature = "script")]
    dsl::assert_both(
        modern,
        "",
        r#"g.V().local(__.union(__.values("age"), __.outE().values("weight")).fold()).mean(Scope.local).toList()"#,
        &[
            Exp::F(7.725),
            Exp::F(27.0),
            Exp::F(11.133333333333333),
            Exp::F(17.6),
        ],
    );
}

/// Null elements of a list are skipped by the reducers (upstream
/// `g.inject([null,10,5,null]).sum(local)` → 15), on a stored list here; the injected form is
/// covered in `inject_tests.rs`.
#[test]
fn reducers_skip_null_elements() {
    let graph = GraphSource::empty();
    {
        let mut lock = graph.write();
        lock.traversal_mut()
            .add_v("data")
            .property(
                "list",
                ElementProperty::Array(vec![
                    ElementProperty::Null,
                    10_i64.into(),
                    5_i64.into(),
                    ElementProperty::Null,
                ]),
            )
            .next()
            .expect("add vertex");
    }
    let lock = graph.read();
    let g = || lock.traversal().v(None).values("list");
    assert_rust(g().sum_scoped(Scope::Local).to_list(), &[Exp::I(15)]);
    assert_rust(g().mean_scoped(Scope::Local).to_list(), &[Exp::F(7.5)]);
    assert_rust(g().min_scoped(Scope::Local).to_list(), &[Exp::I(5)]);
    assert_rust(g().max_scoped(Scope::Local).to_list(), &[Exp::I(10)]);
    #[cfg(feature = "script")]
    dsl::assert_both(
        empty,
        r#"g.addV("data").property("list", [(), 10, 5, ()]).next();
"#,
        r#"g.V().values("list").sum(Scope.local).toList()"#,
        &[Exp::I(15)],
    );
}

#[test]
fn reducer_profile_names() {
    let graph = modern();
    let lock = graph.read();
    for (profile, name) in [
        (
            lock.traversal()
                .v(None)
                .values("age")
                .fold()
                .sum_scoped(Scope::Local)
                .profile(),
            "sum(Scope.local)",
        ),
        (
            lock.traversal()
                .v(None)
                .values("age")
                .fold()
                .min_scoped(Scope::Local)
                .profile(),
            "min(Scope.local)",
        ),
        (
            lock.traversal()
                .v(None)
                .values("age")
                .fold()
                .max_scoped(Scope::Local)
                .profile(),
            "max(Scope.local)",
        ),
        (
            lock.traversal()
                .v(None)
                .values("age")
                .fold()
                .mean_scoped(Scope::Local)
                .profile(),
            "mean(Scope.local)",
        ),
    ] {
        let text = profile.unwrap().to_string();
        assert!(text.contains(name), "{text}");
    }
}

// ---------------------------------------------------------------------------------------------
// Scope::Global ≡ unscoped
// ---------------------------------------------------------------------------------------------

#[test]
fn global_scope_is_the_unscoped_step() {
    let graph = modern();
    let lock = graph.read();
    let g = || lock.traversal().v(None).values("age");
    assert_eq!(
        g().count_scoped(Scope::Global).to_list().unwrap(),
        g().count().to_list().unwrap()
    );
    assert_eq!(
        g().sum_scoped(Scope::Global).to_list().unwrap(),
        g().sum().to_list().unwrap()
    );
    assert_eq!(
        g().min_scoped(Scope::Global).to_list().unwrap(),
        g().min().to_list().unwrap()
    );
    assert_eq!(
        g().max_scoped(Scope::Global).to_list().unwrap(),
        g().max().to_list().unwrap()
    );
    assert_eq!(
        g().mean_scoped(Scope::Global).to_list().unwrap(),
        g().mean().to_list().unwrap()
    );
    let profile = g().sum_scoped(Scope::Global).profile().unwrap().to_string();
    assert!(
        profile.contains("sum()") && !profile.contains("sum(local)"),
        "{profile}"
    );

    // A folded list is not a number for the global step: same error as `sum()`.
    let scoped = lock
        .traversal()
        .v(None)
        .values("age")
        .fold()
        .sum_scoped(Scope::Global)
        .to_list();
    let plain = lock
        .traversal()
        .v(None)
        .values("age")
        .fold()
        .sum()
        .to_list();
    assert_eq!(
        scoped.expect_err("global sum of a list").to_string(),
        plain.expect_err("sum of a list").to_string()
    );

    #[cfg(feature = "script")]
    for (step, expected) in [
        ("count", Exp::I(4)),
        ("sum", Exp::I(123)),
        ("min", Exp::I(27)),
        ("max", Exp::I(35)),
        ("mean", Exp::F(30.75)),
    ] {
        for spelling in [
            "Scope.global",
            "Scope::global",
            "Scope.Global",
            "Scope::Global",
        ] {
            let values = dsl::list(
                modern(),
                &format!(r#"g.V().values("age").{step}({spelling}).toList()"#),
            );
            assert_eq!(values.len(), 1, "{step}({spelling})");
            match expected {
                Exp::I(i) => assert_eq!(values[0].as_int().ok(), Some(i), "{step}({spelling})"),
                Exp::F(f) => assert_eq!(values[0].as_float().ok(), Some(f), "{step}({spelling})"),
            }
        }
    }
}

#[cfg(feature = "script")]
#[test]
fn dsl_global_sum_of_a_list_fails_like_sum() {
    use crate::script_host::eval_value;
    use std::sync::Arc;
    let scoped = eval_value(
        Arc::new(modern()),
        r#"g.V().values("age").fold().sum(Scope.global).toList()"#,
    )
    .expect_err("global sum of a list");
    let plain = eval_value(
        Arc::new(modern()),
        r#"g.V().values("age").fold().sum().toList()"#,
    )
    .expect_err("sum of a list");
    // Identical up to the script position suffix, which differs with the query length.
    let without_position =
        |text: String| text.split(" (line ").next().unwrap_or_default().to_string();
    assert_eq!(
        without_position(scoped.to_string()),
        without_position(plain.to_string())
    );
}

// ---------------------------------------------------------------------------------------------
// limit/skip/tail/range(local)
// ---------------------------------------------------------------------------------------------

/// Renders a Rust result like Rhai's `Debug` renders the same value (`"s"`, `29`, `[..]`,
/// `#{"k": v}` with sorted keys), so one expected text checks both APIs.
fn render(value: &ElementProperty) -> String {
    match value {
        ElementProperty::String(s) => format!("{s:?}"),
        ElementProperty::Array(items) => {
            let items: Vec<String> = items.iter().map(render).collect();
            format!("[{}]", items.join(", "))
        }
        ElementProperty::Object(entries) => {
            let mut entries: Vec<(String, String)> = entries
                .iter()
                .map(|(k, v)| (k.to_string(), render(v)))
                .collect();
            entries.sort();
            let entries: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| format!("{k:?}: {v}"))
                .collect();
            format!("#{{{}}}", entries.join(", "))
        }
        other => match other.as_i64() {
            Some(i) => i.to_string(),
            None => format!("{other:?}"),
        },
    }
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

/// Checks a Rust result against `expected` (rendered, any order).
fn assert_rendered(values: TraverserResult<Vec<ElementProperty>>, expected: &[&str]) {
    let values = values.expect("traversal");
    assert_eq!(
        sorted(values.iter().map(render).collect()),
        sorted(expected.iter().map(|s| s.to_string()).collect())
    );
}

/// Checks a Rhai query (with every `Scope.local` spelled `Scope.local` and `Scope::local`)
/// against `expected` (rendered, any order).
#[cfg(feature = "script")]
fn assert_dsl_rendered(graph: fn() -> Graph, setup: &str, script: &str, expected: &[&str]) {
    for spelling in ["Scope.local", "Scope::local"] {
        let query = format!("{setup}{}", script.replace("Scope.local", spelling));
        let values = dsl::list(graph(), &query);
        assert_eq!(
            sorted(values.iter().map(|v| format!("{v:?}")).collect()),
            sorted(expected.iter().map(|s| s.to_string()).collect()),
            "{query}"
        );
    }
}

#[test]
fn range_family_on_select_maps() {
    let graph = modern();
    let lock = graph.read();
    let in_in = || {
        lock.traversal()
            .v(None)
            .as_("a")
            .in_(None)
            .as_("b")
            .in_(None)
            .as_("c")
            .select(vec!["a", "b", "c"])
            .by("name")
    };
    let out_out = || {
        lock.traversal()
            .v(None)
            .as_("a")
            .out(None)
            .as_("b")
            .out(None)
            .as_("c")
            .select(vec!["a", "b", "c"])
            .by("name")
    };
    let cases: Vec<RenderCase> = vec![
        (
            in_in().limit_scoped(Scope::Local, 2).to_list(),
            r#"g.V().as("a").in().as("b").in().as("c").select("a","b","c").by("name").limit(Scope.local, 2).toList()"#,
            &[
                r#"#{"a": "lop", "b": "josh"}"#,
                r#"#{"a": "ripple", "b": "josh"}"#,
            ],
        ),
        (
            in_in().limit_scoped(Scope::Local, 1).to_list(),
            r#"g.V().as("a").in().as("b").in().as("c").select("a","b","c").by("name").limit(Scope.local, 1).toList()"#,
            &[r#"#{"a": "lop"}"#, r#"#{"a": "ripple"}"#],
        ),
        (
            out_out().range_scoped(Scope::Local, 1, 3).to_list(),
            r#"g.V().as("a").out().as("b").out().as("c").select("a","b","c").by("name").range(Scope.local, 1, 3).toList()"#,
            &[
                r#"#{"b": "josh", "c": "lop"}"#,
                r#"#{"b": "josh", "c": "ripple"}"#,
            ],
        ),
        (
            out_out().range_scoped(Scope::Local, 1, 2).to_list(),
            r#"g.V().as("a").out().as("b").out().as("c").select("a","b","c").by("name").range(Scope.local, 1, 2).toList()"#,
            &[r#"#{"b": "josh"}"#, r#"#{"b": "josh"}"#],
        ),
        (
            out_out().tail_scoped(Scope::Local, 2).to_list(),
            r#"g.V().as("a").out().as("b").out().as("c").select("a","b","c").by("name").tail(Scope.local, 2).toList()"#,
            &[
                r#"#{"b": "josh", "c": "lop"}"#,
                r#"#{"b": "josh", "c": "ripple"}"#,
            ],
        ),
        (
            out_out().tail_scoped(Scope::Local, 1).to_list(),
            r#"g.V().as("a").out().as("b").out().as("c").select("a","b","c").by("name").tail(Scope.local, 1).toList()"#,
            &[r#"#{"c": "lop"}"#, r#"#{"c": "ripple"}"#],
        ),
        (
            out_out().skip_scoped(Scope::Local, 2).to_list(),
            r#"g.V().as("a").out().as("b").out().as("c").select("a","b","c").by("name").skip(Scope.local, 2).toList()"#,
            &[r#"#{"c": "lop"}"#, r#"#{"c": "ripple"}"#],
        ),
    ];
    for (rust, _script, expected) in cases {
        assert_rendered(rust, expected);
        #[cfg(feature = "script")]
        assert_dsl_rendered(modern, "", _script, expected);
    }
}

#[test]
fn range_family_on_lists_and_scalars() {
    let graph = modern();
    let lock = graph.read();
    let names = || lock.traversal().v(None).values("name").fold();
    let cases: Vec<RenderCase> = vec![
        (
            names().limit_scoped(Scope::Local, 2).to_list(),
            r#"g.V().values("name").fold().limit(Scope.local, 2).toList()"#,
            &[r#"["marko", "vadas"]"#],
        ),
        (
            names().limit_scoped(Scope::Local, 1).to_list(),
            r#"g.V().values("name").fold().limit(Scope.local, 1).toList()"#,
            &[r#"["marko"]"#],
        ),
        (
            names().skip_scoped(Scope::Local, 4).to_list(),
            r#"g.V().values("name").fold().skip(Scope.local, 4).toList()"#,
            &[r#"["ripple", "peter"]"#],
        ),
        (
            names().tail_scoped(Scope::Local, 1).to_list(),
            r#"g.V().values("name").fold().tail(Scope.local).toList()"#,
            &[r#"["peter"]"#],
        ),
        (
            names().range_scoped(Scope::Local, 4, -1).to_list(),
            r#"g.V().values("name").fold().range(Scope.local, 4, -1).toList()"#,
            &[r#"["ripple", "peter"]"#],
        ),
        (
            lock.traversal()
                .v(None)
                .fold()
                .range_scoped(Scope::Local, 6, 7)
                .to_list(),
            r#"g.V().fold().range(Scope.local, 6, 7).toList()"#,
            &["[]"],
        ),
        (
            lock.traversal()
                .v(None)
                .has_label(vec!["nobody"])
                .fold()
                .limit_scoped(Scope::Local, 2)
                .to_list(),
            r#"g.V().hasLabel("nobody").fold().limit(Scope.local, 2).toList()"#,
            &["[]"],
        ),
        (
            lock.traversal()
                .v("1")
                .values("age")
                .range_scoped(Scope::Local, 20, 30)
                .to_list(),
            r#"g.V("1").values("age").range(Scope.local, 20, 30).toList()"#,
            &["29"],
        ),
        (
            lock.traversal()
                .v("1")
                .values("age")
                .tail_scoped(Scope::Local, 50)
                .to_list(),
            r#"g.V("1").values("age").tail(Scope.local, 50).toList()"#,
            &["29"],
        ),
        (
            lock.traversal()
                .v("1")
                .value_map(vec!["name"])
                .tail_scoped(Scope::Local, 1)
                .to_list(),
            r#"g.V("1").valueMap("name").tail(Scope.local, 1).toList()"#,
            &[r#"#{"name": ["marko"]}"#],
        ),
        (
            lock.traversal()
                .v(None)
                .group()
                .by(By::Label)
                .by("name")
                .limit_scoped(Scope::Local, 1)
                .to_list(),
            r#"g.V().group().by(T.label).by("name").limit(Scope.local, 1).toList()"#,
            &[r#"#{"person": ["marko", "vadas", "josh", "peter"]}"#],
        ),
    ];
    for (rust, _script, expected) in cases {
        assert_rendered(rust, expected);
        #[cfg(feature = "script")]
        assert_dsl_rendered(modern, "", _script, expected);
    }
}

#[test]
fn range_family_on_lazy_collection_properties() {
    let graph = collections_graph();
    let lock = graph.read();
    assert_rendered(
        lock.traversal()
            .v(None)
            .values("list")
            .range_scoped(Scope::Local, 1, 3)
            .to_list(),
        &["[2, 3]"],
    );
    assert_rendered(
        lock.traversal()
            .v(None)
            .values("map")
            .tail_scoped(Scope::Local, 2)
            .to_list(),
        &[r#"#{"b": 2, "c": 3}"#],
    );
    assert_rendered(
        lock.traversal()
            .v(None)
            .values("list")
            .fold()
            .unfold()
            .limit_scoped(Scope::Local, 0)
            .to_list(),
        &["[]"],
    );
    #[cfg(feature = "script")]
    {
        assert_dsl_rendered(
            empty,
            COLLECTIONS_SETUP,
            r#"g.V().values("list").range(Scope.local, 1, 3).toList()"#,
            &["[2, 3]"],
        );
        assert_dsl_rendered(
            empty,
            COLLECTIONS_SETUP,
            r#"g.V().values("map").skip(Scope.local, 1).toList()"#,
            &[r#"#{"b": 2, "c": 3}"#],
        );
        // A constant (`Computed`) list and map.
        assert_dsl_rendered(
            empty,
            "",
            r#"g.inject(0).constant([1, 2, 3]).tail(Scope.local, 2).toList()"#,
            &["[2, 3]"],
        );
    }
}

#[test]
fn range_family_nested_in_by() {
    let graph = modern();
    let lock = graph.read();
    assert_rendered(
        lock.traversal()
            .v(None)
            .as_("a")
            .out(None)
            .as_("a")
            .out(None)
            .as_("a")
            .select(vec!["a"])
            .by(__::unfold().values("name").fold())
            .tail_scoped(Scope::Local, 1)
            .unfold()
            .to_list(),
        &[r#""lop""#, r#""ripple""#],
    );
    assert_rendered(
        lock.traversal()
            .v("1")
            .as_("a")
            .select(vec!["a"])
            .by(__::values("name").fold().limit_scoped(Scope::Local, 0))
            .to_list(),
        &["[]"],
    );
    #[cfg(feature = "script")]
    {
        assert_dsl_rendered(
            modern,
            "",
            r#"g.V().as("a").out().as("a").out().as("a").select("a").by(__.unfold().values("name").fold()).tail(Scope.local).unfold().toList()"#,
            &[r#""lop""#, r#""ripple""#],
        );
        assert_dsl_rendered(
            modern,
            "",
            r#"g.V("1").as("a").select("a").by(__.values("name").fold().tail(Scope.local)).toList()"#,
            &[r#"["marko"]"#],
        );
    }
}

#[test]
fn range_family_rejects_invalid_bounds_and_paths() {
    let graph = modern();
    let lock = graph.read();
    let folded = || lock.traversal().v(None).fold();
    for result in [
        folded().range_scoped(Scope::Local, 3, 1).to_list(),
        folded().range_scoped(Scope::Local, -1, 2).to_list(),
        folded().range_scoped(Scope::Local, 0, -2).to_list(),
        folded().limit_scoped(Scope::Local, -1).to_list(),
        folded().skip_scoped(Scope::Local, -1).to_list(),
        folded().tail_scoped(Scope::Local, -1).to_list(),
    ] {
        let err = result.expect_err("invalid bounds");
        let cause = err.root_cause().to_string();
        assert!(cause.contains("Invalid range"), "{cause}");
    }
    let err = lock
        .traversal()
        .v(None)
        .out(None)
        .path()
        .limit_scoped(Scope::Local, 1)
        .to_list()
        .expect_err("path input");
    let cause = err.root_cause().to_string();
    assert!(cause.contains("limit(local) on a path"), "{cause}");
    #[cfg(feature = "script")]
    {
        use crate::script_host::eval_value;
        use std::sync::Arc;
        for query in [
            r#"g.V().fold().range(Scope.local, 3, 1).toList()"#,
            r#"g.V().fold().limit(Scope::local, -1).toList()"#,
            r#"g.V().fold().tail(Scope.local, -2).toList()"#,
        ] {
            let err = eval_value(Arc::new(modern()), query).expect_err(query);
            assert!(err.to_string().contains("Invalid range"), "{query}: {err}");
        }
        let err = eval_value(
            Arc::new(modern()),
            r#"g.V().out().path().range(Scope.local, 0, 1).toList()"#,
        )
        .expect_err("path input");
        assert!(err.to_string().contains("range(local) on a path"), "{err}");
    }
}

#[test]
fn range_family_global_scope_is_the_unscoped_step() {
    let graph = modern();
    let lock = graph.read();
    let g = || lock.traversal().v(None).values("name").order();
    assert_eq!(
        g().limit_scoped(Scope::Global, 2).to_list().unwrap(),
        g().limit(2).to_list().unwrap()
    );
    assert_eq!(
        g().skip_scoped(Scope::Global, 2).to_list().unwrap(),
        g().skip(2).to_list().unwrap()
    );
    assert_eq!(
        g().tail_scoped(Scope::Global, 2).to_list().unwrap(),
        g().tail(2).to_list().unwrap()
    );
    assert_eq!(
        g().range_scoped(Scope::Global, 1, 3).to_list().unwrap(),
        g().range(1, 3).to_list().unwrap()
    );
    assert_rendered(
        g().tail_scoped(Scope::Global, 2).to_list(),
        &[r#""ripple""#, r#""vadas""#],
    );
    #[cfg(feature = "script")]
    for spelling in ["Scope.global", "Scope::Global"] {
        for (query, expected) in [
            (
                format!(r#"g.V().values("name").order().tail({spelling}, 2).toList()"#),
                vec![r#""ripple""#, r#""vadas""#],
            ),
            (
                format!(r#"g.V().values("name").order().tail({spelling}).toList()"#),
                vec![r#""vadas""#],
            ),
            (
                format!(r#"g.V().values("name").order().limit({spelling}, 1).toList()"#),
                vec![r#""josh""#],
            ),
            (
                format!(r#"g.V().values("name").order().skip({spelling}, 5).toList()"#),
                vec![r#""vadas""#],
            ),
            (
                format!(r#"g.V().values("name").order().range({spelling}, 0, 1).toList()"#),
                vec![r#""josh""#],
            ),
        ] {
            let values = dsl::list(modern(), &query);
            let rendered: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
            assert_eq!(rendered, expected, "{query}");
        }
    }
}

#[test]
fn range_family_profile_names_and_no_rule_fires() {
    let graph = modern();
    let lock = graph.read();
    let folded = || lock.traversal().v(None).values("name").fold();
    for (profile, name) in [
        (
            folded().limit_scoped(Scope::Local, 2).profile(),
            "limit(Scope.local, 2)",
        ),
        (
            folded().skip_scoped(Scope::Local, 2).profile(),
            "skip(Scope.local, 2)",
        ),
        (
            folded().tail_scoped(Scope::Local, 1).profile(),
            "tail(Scope.local, 1)",
        ),
        (
            folded().range_scoped(Scope::Local, 1, 3).profile(),
            "range(Scope.local, 1, 3)",
        ),
    ] {
        let text = profile.unwrap().to_string();
        assert!(text.contains(name), "{text}");
        assert!(!text.contains("Optimizer rules applied"), "{text}");
    }
}

// ---------------------------------------------------------------------------------------------
// order(local), by(Column), dedup(local)
// ---------------------------------------------------------------------------------------------

/// Checks a Rust result against `expected` (rendered, in order).
fn assert_rendered_in_order(values: TraverserResult<Vec<ElementProperty>>, expected: &[&str]) {
    let values = values.expect("traversal");
    let rendered: Vec<String> = values.iter().map(render).collect();
    assert_eq!(rendered, expected);
}

/// Checks a Rhai query (with `Scope.local` spelled both ways) against `expected` (rendered, in
/// order).
#[cfg(feature = "script")]
fn assert_dsl_in_order(graph: fn() -> Graph, script: &str, expected: &[&str]) {
    for spelling in ["Scope.local", "Scope::local"] {
        let query = script.replace("Scope.local", spelling);
        let values = dsl::list(graph(), &query);
        let rendered: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
        assert_eq!(rendered, expected, "{query}");
    }
}

#[test]
fn order_local_sorts_each_list() {
    let graph = modern();
    let lock = graph.read();
    let g = || lock.traversal();
    let cases: Vec<RenderCase> = vec![
        (
            g().v(None)
                .has_label("person")
                .fold()
                .order_scoped(Scope::Local)
                .by("age")
                .unfold()
                .values("name")
                .to_list(),
            r#"g.V().hasLabel("person").fold().order(Scope.local).by("age").unfold().values("name").toList()"#,
            &[r#""vadas""#, r#""marko""#, r#""josh""#, r#""peter""#],
        ),
        // Software vertices have no `age`: the unproductive `by()` drops them from the list.
        (
            g().v(None)
                .fold()
                .order_scoped(Scope::Local)
                .by("age")
                .unfold()
                .values("name")
                .to_list(),
            r#"g.V().fold().order(Scope.local).by("age").unfold().values("name").toList()"#,
            &[r#""vadas""#, r#""marko""#, r#""josh""#, r#""peter""#],
        ),
        (
            g().v(None)
                .fold()
                .order_scoped(Scope::Local)
                .by("age")
                .by(Order::Desc)
                .unfold()
                .values("name")
                .to_list(),
            r#"g.V().fold().order(Scope.local).by("age", Order.desc).unfold().values("name").toList()"#,
            &[r#""peter""#, r#""josh""#, r#""marko""#, r#""vadas""#],
        ),
        // Natural order, and a bare direction.
        (
            g().v(None)
                .values("age")
                .fold()
                .order_scoped(Scope::Local)
                .to_list(),
            r#"g.V().values("age").fold().order(Scope.local).toList()"#,
            &["[27, 29, 32, 35]"],
        ),
        (
            g().v(None)
                .values("age")
                .fold()
                .order_scoped(Scope::Local)
                .by(Order::Desc)
                .to_list(),
            r#"g.V().values("age").fold().order(Scope.local).by(Order.desc).toList()"#,
            &["[35, 32, 29, 27]"],
        ),
        // A scalar passes through.
        (
            g().v("1")
                .values("age")
                .order_scoped(Scope::Local)
                .to_list(),
            r#"g.V("1").values("age").order(Scope.local).toList()"#,
            &["29"],
        ),
        // Stable: every person has the same label, so the input order is kept.
        (
            g().v(None)
                .has_label("person")
                .fold()
                .order_scoped(Scope::Local)
                .by(By::Label)
                .unfold()
                .values("name")
                .to_list(),
            r#"g.V().hasLabel("person").fold().order(Scope.local).by(T.label).unfold().values("name").toList()"#,
            &[r#""marko""#, r#""vadas""#, r#""josh""#, r#""peter""#],
        ),
        // Tiebreakers: label ascending, then name descending.
        (
            g().v(None)
                .fold()
                .order_scoped(Scope::Local)
                .by(By::Label)
                .by("name")
                .by(Order::Desc)
                .unfold()
                .values("name")
                .to_list(),
            r#"g.V().fold().order(Scope.local).by(T.label).by("name", Order.desc).unfold().values("name").toList()"#,
            &[
                r#""vadas""#,
                r#""peter""#,
                r#""marko""#,
                r#""josh""#,
                r#""ripple""#,
                r#""lop""#,
            ],
        ),
        (
            g().v(None)
                .has_label("person")
                .fold()
                .order_scoped(Scope::Local)
                .by(__::out_e(None).count())
                .by(Order::Desc)
                .by("name")
                .unfold()
                .values("name")
                .to_list(),
            r#"g.V().hasLabel("person").fold().order(Scope.local).by(__.outE().count(), Order.desc).by("name").unfold().values("name").toList()"#,
            &[r#""marko""#, r#""josh""#, r#""peter""#, r#""vadas""#],
        ),
    ];
    for (rust, _script, expected) in cases {
        assert_rendered_in_order(rust, expected);
        #[cfg(feature = "script")]
        assert_dsl_in_order(modern, _script, expected);
    }
}

/// `skip(local)` on a locally sorted list (filter/Range.feature).
#[test]
fn order_local_then_skip_local() {
    let graph = modern();
    let lock = graph.read();
    let rust = lock
        .traversal()
        .v(None)
        .out_e(None)
        .values("weight")
        .fold()
        .order_scoped(Scope::Local)
        .skip_scoped(Scope::Local, 2)
        .to_list()
        .expect("traversal");
    let weights: Vec<f64> = match rust.as_slice() {
        [ElementProperty::Array(items)] => {
            items.iter().filter_map(ElementProperty::as_f64).collect()
        }
        other => panic!("expected one list, got {other:?}"),
    };
    assert_eq!(weights, [0.4, 0.5, 1.0, 1.0]);
    #[cfg(feature = "script")]
    assert_dsl_in_order(
        modern,
        r#"g.V().outE().values("weight").fold().order(Scope.local).skip(Scope.local, 2).toList()"#,
        &["[0.4, 0.5, 1.0, 1.0]"],
    );
}

#[test]
fn order_local_sorts_map_entries_by_column() {
    let graph = modern();
    let lock = graph.read();
    let g = || lock.traversal();
    let weights = || {
        g().v(None)
            .has_label("person")
            .group()
            .by("name")
            .by(__::out_e(None).values("weight").sum())
    };
    let cases: Vec<RenderCase> = vec![
        (
            g().v("1")
                .element_map(None::<()>)
                .order_scoped(Scope::Local)
                .by(Column::Keys)
                .by(Order::Asc)
                .unfold()
                .to_list(),
            r#"g.V("1").elementMap().order(Scope.local).by(Column.keys, Order.asc).unfold().toList()"#,
            &[
                r#"#{"age": 29}"#,
                r#"#{"id": "1"}"#,
                r#"#{"label": "person"}"#,
                r#"#{"name": "marko"}"#,
            ],
        ),
        (
            g().v("1")
                .element_map(None::<()>)
                .order_scoped(Scope::Local)
                .by(Column::Keys)
                .by(Order::Desc)
                .unfold()
                .to_list(),
            r#"g.V("1").elementMap().order(Scope.local).by(Column::keys, Order.desc).unfold().toList()"#,
            &[
                r#"#{"name": "marko"}"#,
                r#"#{"label": "person"}"#,
                r#"#{"id": "1"}"#,
                r#"#{"age": 29}"#,
            ],
        ),
        (
            weights()
                .order_scoped(Scope::Local)
                .by(Column::Values)
                .unfold()
                .to_list(),
            r#"g.V().hasLabel("person").group().by("name").by(__.outE().values("weight").sum()).order(Scope.local).by(Column.values).unfold().toList()"#,
            &[
                r#"#{"peter": 0.2}"#,
                r#"#{"josh": 1.4}"#,
                r#"#{"marko": 1.9}"#,
            ],
        ),
        // Global `order()` of the entries of an unfolded map.
        (
            weights()
                .unfold()
                .order()
                .by(Column::Values)
                .by(Order::Desc)
                .to_list(),
            r#"g.V().hasLabel("person").group().by("name").by(__.outE().values("weight").sum()).unfold().order().by(Column.values, Order.desc).toList()"#,
            &[
                r#"#{"marko": 1.9}"#,
                r#"#{"josh": 1.4}"#,
                r#"#{"peter": 0.2}"#,
            ],
        ),
        (
            weights().unfold().order().by(Column::Keys).to_list(),
            r#"g.V().hasLabel("person").group().by("name").by(__.outE().values("weight").sum()).unfold().order().by(Column.keys).toList()"#,
            &[
                r#"#{"josh": 1.4}"#,
                r#"#{"marko": 1.9}"#,
                r#"#{"peter": 0.2}"#,
            ],
        ),
    ];
    for (rust, _script, expected) in cases {
        assert_rendered_in_order(rust, expected);
        #[cfg(feature = "script")]
        assert_dsl_in_order(modern, _script, expected);
    }
    // The sorted map stays a map, and a `select()` record keeps its keys.
    let sorted = weights()
        .order_scoped(Scope::Local)
        .by(Column::Values)
        .to_list();
    assert_rendered(sorted, &[r#"#{"josh": 1.4, "marko": 1.9, "peter": 0.2}"#]);
    // `by(Column)` on a value that is neither a map nor an entry is unproductive: dropped.
    let names = g()
        .v(None)
        .values("name")
        .fold()
        .order_scoped(Scope::Local)
        .by(Column::Keys)
        .to_list();
    assert_rendered(names, &["[]"]);
}

/// `order(local)` inside a `select()` `by()` (map/Select.feature).
#[test]
fn order_local_nested_in_select_by() {
    let graph = modern();
    let lock = graph.read();
    let rust = lock
        .traversal()
        .v(None)
        .has_label("software")
        .as_("name")
        .as_("language")
        .as_("creators")
        .select(vec!["name", "language", "creators"])
        .by("name")
        .by("lang")
        .by(__::in_("created")
            .values("name")
            .fold()
            .order_scoped(Scope::Local))
        .to_list();
    let expected = [
        r#"#{"creators": ["josh", "marko", "peter"], "language": "java", "name": "lop"}"#,
        r#"#{"creators": ["josh"], "language": "java", "name": "ripple"}"#,
    ];
    assert_rendered(rust, &expected);
    #[cfg(feature = "script")]
    assert_dsl_rendered(
        modern,
        "",
        r#"g.V().hasLabel("software").as("name").as("language").as("creators").select("name","language","creators").by("name").by("lang").by(__.in("created").values("name").fold().order(Scope.local)).toList()"#,
        &expected,
    );
}

/// A `by(__.select("x"))` child of `order(local)` reads the outer traverser's path: the path
/// analysis records `x` for it (rule 13), and every element resolves the label.
#[test]
fn order_local_by_child_reads_the_outer_path() {
    let graph = modern();
    let lock = graph.read();
    let build = || {
        lock.traversal()
            .v("1")
            .as_("x")
            .project(vec!["names"])
            .by(__::out(None).values("name").fold())
            .select(vec!["names"])
            .order_scoped(Scope::Local)
            .by(__::select(vec!["x"]).values("age"))
            .by(__::identity())
            .by(Order::Desc)
    };
    assert_rendered_in_order(build().to_list(), &[r#"["vadas", "lop", "josh"]"#]);
    let profile = build().profile().expect("profile").to_string();
    assert!(profile.contains("as(\"x\") [path: labels(x)]"), "{profile}");
    assert!(profile.contains("order(Scope.local).by("), "{profile}");
    #[cfg(feature = "script")]
    assert_dsl_in_order(
        modern,
        r#"g.V("1").as("x").project("names").by(__.out().values("name").fold()).select("names").order(Scope.local).by(__.select("x").values("age")).by(__.identity(), Order.desc).toList()"#,
        &[r#"["vadas", "lop", "josh"]"#],
    );
}

#[test]
fn order_global_scope_is_the_unscoped_step() {
    let graph = modern();
    let lock = graph.read();
    let scoped = lock
        .traversal()
        .v(None)
        .values("age")
        .order_scoped(Scope::Global)
        .to_list();
    let plain = lock.traversal().v(None).values("age").order().to_list();
    assert_eq!(scoped.expect("scoped"), plain.expect("plain"));
    let folded = lock
        .traversal()
        .v(None)
        .values("age")
        .fold()
        .order_scoped(Scope::Global)
        .to_list();
    assert_rendered(folded, &["[29, 27, 32, 35]"]);
    #[cfg(feature = "script")]
    {
        let values = dsl::list(
            modern(),
            r#"g.V().values("age").order(Scope.global).toList()"#,
        );
        let rendered: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
        assert_eq!(rendered, ["27", "29", "32", "35"]);
    }
}

#[test]
fn dedup_local_removes_duplicate_list_elements() {
    let graph = modern();
    let lock = graph.read();
    let g = || lock.traversal();
    let cases: Vec<RenderCase> = vec![
        (
            g().v(None)
                .out(None)
                .in_(None)
                .values("name")
                .fold()
                .dedup_scoped(Scope::Local)
                .unfold()
                .to_list(),
            r#"g.V().out().in().values("name").fold().dedup(Scope.local).unfold().toList()"#,
            &[r#""marko""#, r#""peter""#, r#""josh""#],
        ),
        (
            g().v("1")
                .values("age")
                .dedup_scoped(Scope::Local)
                .unfold()
                .to_list(),
            r#"g.V("1").values("age").dedup(Scope.local).unfold().toList()"#,
            &["29"],
        ),
        // Vertex handles survive: dedup by identity, then read their names.
        (
            g().v(None)
                .out(None)
                .fold()
                .dedup_scoped(Scope::Local)
                .unfold()
                .values("name")
                .to_list(),
            r#"g.V().out().fold().dedup(Scope.local).unfold().values("name").toList()"#,
            &[r#""josh""#, r#""lop""#, r#""vadas""#, r#""ripple""#],
        ),
        // A map is not a list: it passes through unchanged.
        (
            g().v("1")
                .value_map(vec!["name"])
                .dedup_scoped(Scope::Local)
                .to_list(),
            r#"g.V("1").valueMap("name").dedup(Scope.local).toList()"#,
            &[r#"#{"name": ["marko"]}"#],
        ),
    ];
    for (rust, _script, expected) in cases {
        assert_rendered_in_order(rust, expected);
        #[cfg(feature = "script")]
        assert_dsl_in_order(modern, _script, expected);
    }
}

/// `dedup(Scope.local, "x", "y")` ignores the labels (TinkerPop hands them only to the global
/// step): whole `{x, y}` maps are deduplicated.
#[test]
fn dedup_local_ignores_labels() {
    let graph = modern();
    let lock = graph.read();
    let rust = lock
        .traversal()
        .v(None)
        .out(None)
        .as_("x")
        .in_(None)
        .as_("y")
        .select(vec!["x", "y"])
        .by("name")
        .fold()
        .dedup_scoped_labels(Scope::Local, ["x", "y"])
        .unfold()
        .to_list();
    let expected = [
        r#"#{"x": "josh", "y": "marko"}"#,
        r#"#{"x": "lop", "y": "josh"}"#,
        r#"#{"x": "lop", "y": "marko"}"#,
        r#"#{"x": "lop", "y": "peter"}"#,
        r#"#{"x": "ripple", "y": "josh"}"#,
        r#"#{"x": "vadas", "y": "marko"}"#,
    ];
    assert_rendered(rust, &expected);
    #[cfg(feature = "script")]
    assert_dsl_rendered(
        modern,
        "",
        r#"g.V().out().as("x").in().as("y").select("x","y").by("name").fold().dedup(Scope.local, "x", "y").unfold().toList()"#,
        &expected,
    );
}

#[test]
fn dedup_global_scope_is_the_unscoped_step_and_keys_by_labels() {
    let graph = modern();
    let lock = graph.read();
    let scoped = lock
        .traversal()
        .v(None)
        .out(None)
        .dedup_scoped(Scope::Global)
        .id()
        .to_list();
    let plain = lock.traversal().v(None).out(None).dedup().id().to_list();
    assert_eq!(scoped.expect("scoped"), plain.expect("plain"));
    // With labels the key is the labelled path object (TinkerPop's `dedup("a")`): one
    // survivor per start vertex with an outgoing edge.
    let by_label = lock
        .traversal()
        .v(None)
        .as_("a")
        .out(None)
        .dedup_scoped_labels(Scope::Global, ["a"])
        .count()
        .next()
        .expect("global dedup by labels");
    assert_eq!(by_label.and_then(|c| c.as_i64()), Some(3));
    #[cfg(feature = "script")]
    {
        let values = dsl::list(
            modern(),
            r#"g.V().out().dedup(Scope.global).count().toList()"#,
        );
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].as_int().ok(), Some(4));
        let values = dsl::list(
            modern(),
            r#"g.V().as("a").out().dedup(Scope.global, "a").count().toList()"#,
        );
        assert_eq!(values[0].as_int().ok(), Some(3));
    }
}

#[test]
fn order_and_dedup_local_profile_names_and_no_rule_fires() {
    let graph = modern();
    let lock = graph.read();
    let folded = || lock.traversal().v(None).fold();
    for (profile, name) in [
        (
            folded().order_scoped(Scope::Local).by("age").profile(),
            "order(Scope.local).by(\"age\")",
        ),
        (
            folded().dedup_scoped(Scope::Local).profile(),
            "dedup(Scope.local)",
        ),
    ] {
        let text = profile.unwrap().to_string();
        assert!(text.contains(name), "{text}");
        assert!(!text.contains("Optimizer rules applied"), "{text}");
    }
}

/// `by(Column)` is a sort key only `order()` takes; `group()` rejects it.
#[test]
fn by_column_is_rejected_outside_order() {
    let graph = modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v(None)
        .group()
        .by(Column::Keys)
        .to_list()
        .expect_err("group().by(Column.keys)");
    match err.root_cause() {
        TraverserError::InvalidModulator { step, position, .. } => {
            assert_eq!(step, "group");
            assert_eq!(*position, 1);
        }
        other => panic!("expected InvalidModulator, got {other}"),
    }
    let help = err.help().expect("help");
    assert!(
        help.contains("order().by(Column.values, Order.desc)"),
        "{help}"
    );
    let err = lock
        .traversal()
        .v(None)
        .dedup()
        .by(Column::Values)
        .to_list()
        .expect_err("dedup().by(Column.values)");
    assert!(
        matches!(err.root_cause(), TraverserError::InvalidModulator { .. }),
        "{err}"
    );
}

// ---------------------------------------------------------------------------------------------
// String steps: toUpper/toLower/trim/lTrim/rTrim/length/asString/replace/substring(local)
// ---------------------------------------------------------------------------------------------

/// Like [`render`], but a `null` renders as Rhai's `()`, so a list with a null element compares
/// against both APIs.
fn render_nullable(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "()".to_string(),
        ElementProperty::Array(items) => {
            let items: Vec<String> = items.iter().map(render_nullable).collect();
            format!("[{}]", items.join(", "))
        }
        other => render(other),
    }
}

/// A graph whose single vertex holds a list of strings with a `null` element (`words`), a
/// list mixing a string and a number (`mixed`) and a list with a `null` for `as_string` (`nums`).
fn strings_graph() -> Graph {
    let graph = GraphSource::empty();
    {
        let mut lock = graph.write();
        lock.traversal_mut()
            .add_v("data")
            .property(
                "words",
                ElementProperty::Array(vec![
                    " Hello ".into(),
                    ElementProperty::Null,
                    "wOrLd".into(),
                ]),
            )
            .property(
                "mixed",
                ElementProperty::Array(vec!["a".into(), 1_i64.into()]),
            )
            .property(
                "nums",
                ElementProperty::Array(vec![1_i64.into(), ElementProperty::Null]),
            )
            .next()
            .expect("add data vertex");
    }
    graph
}

#[cfg(feature = "script")]
const STRINGS_SETUP: &str = r#"g.addV("data").property("words", [" Hello ", (), "wOrLd"]).property("mixed", ["a", 1]).property("nums", [1, ()]).next();
"#;

/// Every string step with `Scope.local` on a folded, ordered list of the modern names, in Rust
/// and in the DSL (both step spellings, both token spellings).
#[test]
fn string_steps_on_a_folded_list() {
    let graph = modern();
    let lock = graph.read();
    let names = || lock.traversal().v(None).values("name").order().fold();
    let software = || {
        lock.traversal()
            .v(None)
            .has_label("software")
            .values("name")
            .order()
            .fold()
    };
    let ages = || {
        lock.traversal()
            .v(None)
            .has_label("person")
            .values("age")
            .order()
            .fold()
    };
    let upper = r#"["JOSH", "LOP", "MARKO", "PETER", "RIPPLE", "VADAS"]"#;
    let lower = r#"["josh", "lop", "marko", "peter", "ripple", "vadas"]"#;
    assert_rendered(names().to_upper_scoped(Scope::Local).to_list(), &[upper]);
    assert_rendered(names().to_lower_scoped(Scope::Local).to_list(), &[lower]);
    assert_rendered(names().trim_scoped(Scope::Local).to_list(), &[lower]);
    assert_rendered(names().l_trim_scoped(Scope::Local).to_list(), &[lower]);
    assert_rendered(names().r_trim_scoped(Scope::Local).to_list(), &[lower]);
    assert_rendered(
        names().length_scoped(Scope::Local).to_list(),
        &["[4, 3, 5, 5, 6, 5]"],
    );
    assert_rendered(
        ages().as_string_scoped(Scope::Local).to_list(),
        &[r#"["27", "29", "32", "35"]"#],
    );
    assert_rendered(
        software().replace_scoped(Scope::Local, "p", "g").to_list(),
        &[r#"["log", "riggle"]"#],
    );
    assert_rendered(
        software().substring_scoped(Scope::Local, 2, None).to_list(),
        &[r#"["p", "pple"]"#],
    );
    assert_rendered(
        software()
            .substring_scoped(Scope::Local, 1, Some(4))
            .to_list(),
        &[r#"["op", "ipp"]"#],
    );
    assert_rendered(
        software()
            .substring_scoped(Scope::Local, -3, None)
            .to_list(),
        &[r#"["lop", "ple"]"#],
    );

    #[cfg(feature = "script")]
    {
        let names = r#"g.V().values("name").order().fold()"#;
        let software = r#"g.V().hasLabel("software").values("name").order().fold()"#;
        for (script, expected) in [
            (format!("{names}.toUpper(Scope.local).toList()"), upper),
            (format!("{names}.to_upper(Scope.local).toList()"), upper),
            (format!("{names}.toLower(Scope.local).toList()"), lower),
            (format!("{names}.to_lower(Scope.local).toList()"), lower),
            (format!("{names}.trim(Scope.local).toList()"), lower),
            (format!("{names}.lTrim(Scope.local).toList()"), lower),
            (format!("{names}.l_trim(Scope.local).toList()"), lower),
            (format!("{names}.rTrim(Scope.local).toList()"), lower),
            (format!("{names}.r_trim(Scope.local).toList()"), lower),
            (format!("{names}.length(Scope.local).toList()"), "[4, 3, 5, 5, 6, 5]"),
            (
                r#"g.V().hasLabel("person").values("age").order().fold().asString(Scope.local).toList()"#
                    .to_string(),
                r#"["27", "29", "32", "35"]"#,
            ),
            (
                r#"g.V().hasLabel("person").values("age").order().fold().as_string(Scope.local).toList()"#
                    .to_string(),
                r#"["27", "29", "32", "35"]"#,
            ),
            (
                format!(r#"{software}.replace(Scope.local, "p", "g").toList()"#),
                r#"["log", "riggle"]"#,
            ),
            (format!("{software}.substring(Scope.local, 2).toList()"), r#"["p", "pple"]"#),
            (format!("{software}.substring(Scope.local, 1, 4).toList()"), r#"["op", "ipp"]"#),
            (format!("{software}.substring(Scope.local, -3).toList()"), r#"["lop", "ple"]"#),
        ] {
            assert_dsl_rendered(modern, "", &script, &[expected]);
        }
    }
}

/// A single string under `Scope.local` is transformed exactly as by the unscoped step.
#[test]
fn string_steps_on_a_scalar_string() {
    let graph = modern();
    let lock = graph.read();
    let names = || lock.traversal().v(None).values("name");
    let upper = ["MARKO", "VADAS", "LOP", "JOSH", "RIPPLE", "PETER"].map(|s| format!("{s:?}"));
    let upper: Vec<&str> = upper.iter().map(String::as_str).collect();
    assert_rendered(names().to_upper_scoped(Scope::Local).to_list(), &upper);
    assert_rendered(
        names().length_scoped(Scope::Local).to_list(),
        &["5", "5", "3", "4", "6", "5"],
    );
    assert_rendered(
        lock.traversal()
            .v(None)
            .has_label("person")
            .values("age")
            .as_string_scoped(Scope::Local)
            .to_list(),
        &[r#""29""#, r#""27""#, r#""32""#, r#""35""#],
    );
    assert_rendered(
        lock.traversal()
            .v(None)
            .has_label("person")
            .values("name")
            .substring_from(-3)
            .to_list(),
        &[r#""rko""#, r#""das""#, r#""osh""#, r#""ter""#],
    );
    #[cfg(feature = "script")]
    {
        assert_dsl_rendered(
            modern,
            "",
            r#"g.V().values("name").toUpper(Scope.local).toList()"#,
            &upper,
        );
        assert_dsl_rendered(
            modern,
            "",
            r#"g.V().hasLabel("software").values("name").substring(2).toList()"#,
            &[r#""p""#, r#""pple""#],
        );
        assert_dsl_rendered(
            modern,
            "",
            r#"g.V().hasLabel("person").values("name").substring(Scope.local, -3).toList()"#,
            &[r#""rko""#, r#""das""#, r#""osh""#, r#""ter""#],
        );
    }
}

/// A `null` list element stays `null`, and a `null` input stays `null`.
#[test]
fn string_steps_keep_null_elements() {
    let graph = strings_graph();
    let lock = graph.read();
    let words = || lock.traversal().v(None).values("words");
    let rendered = |values: TraverserResult<Vec<ElementProperty>>| -> Vec<String> {
        values
            .expect("traversal")
            .iter()
            .map(render_nullable)
            .collect()
    };
    for (values, expected) in [
        (
            rendered(words().to_upper_scoped(Scope::Local).to_list()),
            r#"[" HELLO ", (), "WORLD"]"#,
        ),
        (
            rendered(words().to_lower_scoped(Scope::Local).to_list()),
            r#"[" hello ", (), "world"]"#,
        ),
        (
            rendered(words().trim_scoped(Scope::Local).to_list()),
            r#"["Hello", (), "wOrLd"]"#,
        ),
        (
            rendered(words().l_trim_scoped(Scope::Local).to_list()),
            r#"["Hello ", (), "wOrLd"]"#,
        ),
        (
            rendered(words().r_trim_scoped(Scope::Local).to_list()),
            r#"[" Hello", (), "wOrLd"]"#,
        ),
        (
            rendered(words().length_scoped(Scope::Local).to_list()),
            "[7, (), 5]",
        ),
        (
            rendered(words().replace_scoped(Scope::Local, "l", "L").to_list()),
            r#"[" HeLLo ", (), "wOrLd"]"#,
        ),
        (
            rendered(words().substring_scoped(Scope::Local, 1, Some(3)).to_list()),
            r#"["He", (), "Or"]"#,
        ),
        (
            rendered(
                lock.traversal()
                    .v(None)
                    .constant(ElementProperty::Null)
                    .to_upper_scoped(Scope::Local)
                    .to_list(),
            ),
            "()",
        ),
    ] {
        assert_eq!(values, vec![expected.to_string()]);
    }

    #[cfg(feature = "script")]
    {
        for (script, expected) in [
            (
                r#"g.V().values("words").toUpper(Scope.local).toList()"#,
                r#"[" HELLO ", (), "WORLD"]"#,
            ),
            (
                r#"g.V().values("words").trim(Scope.local).toList()"#,
                r#"["Hello", (), "wOrLd"]"#,
            ),
            (
                r#"g.V().values("words").length(Scope.local).toList()"#,
                "[7, (), 5]",
            ),
            (
                r#"g.V().values("words").replace(Scope.local, "l", "L").toList()"#,
                r#"[" HeLLo ", (), "wOrLd"]"#,
            ),
            (r#"g.V().constant(()).toLower(Scope.local).toList()"#, "()"),
        ] {
            assert_dsl_rendered(empty, STRINGS_SETUP, script, &[expected]);
        }
    }
}

/// A non-string list element fails with TinkerPop's wording; `as_string(local)` fails on a
/// `null` element instead.
#[test]
fn string_steps_reject_non_string_elements() {
    let graph = strings_graph();
    let lock = graph.read();
    let mixed = || lock.traversal().v(None).values("mixed");
    for (result, gremlin) in [
        (mixed().to_upper_scoped(Scope::Local).to_list(), "toUpper"),
        (mixed().to_lower_scoped(Scope::Local).to_list(), "toLower"),
        (mixed().trim_scoped(Scope::Local).to_list(), "trim"),
        (mixed().l_trim_scoped(Scope::Local).to_list(), "lTrim"),
        (mixed().r_trim_scoped(Scope::Local).to_list(), "rTrim"),
        (mixed().length_scoped(Scope::Local).to_list(), "length"),
        (
            mixed().replace_scoped(Scope::Local, "a", "b").to_list(),
            "replace",
        ),
        (
            mixed().substring_scoped(Scope::Local, 0, None).to_list(),
            "substring",
        ),
    ] {
        let err = result.expect_err(gremlin);
        let expected = format!("The {gremlin}(local) step can only take string or list of strings");
        assert!(err.root_cause().to_string().contains(&expected), "{err}");
        match err.root_cause() {
            TraverserError::Value(ValueError::LocalElementType { index, actual, .. }) => {
                assert_eq!(*index, 1);
                assert_eq!(actual, "int64");
            }
            other => panic!("expected LocalElementType, got {other}"),
        }
    }
    // A folded stream of vertices: a stream element that is no string at all.
    let err = lock
        .traversal()
        .v(None)
        .fold()
        .trim_scoped(Scope::Local)
        .to_list()
        .expect_err("vertices are not strings");
    assert!(
        err.root_cause()
            .to_string()
            .contains("The trim(local) step can only take string or list of strings"),
        "{err}"
    );
    // `as_string(local)` converts numbers but not `null`.
    let err = lock
        .traversal()
        .v(None)
        .values("nums")
        .as_string_scoped(Scope::Local)
        .to_list()
        .expect_err("null has no string form");
    assert!(
        err.root_cause().to_string().contains("Can't parse"),
        "{err}"
    );
    // A map is not a list: the same cast error as the unscoped step.
    let modern_graph = modern();
    let modern_lock = modern_graph.read();
    let local = modern_lock
        .traversal()
        .v(None)
        .value_map("name")
        .to_upper_scoped(Scope::Local)
        .to_list()
        .expect_err("a map is not a string");
    let global = modern_lock
        .traversal()
        .v(None)
        .value_map("name")
        .to_upper()
        .to_list()
        .expect_err("a map is not a string");
    assert_eq!(
        local.root_cause().to_string(),
        global.root_cause().to_string()
    );

    #[cfg(feature = "script")]
    {
        use crate::script_host::eval_value;
        use std::sync::Arc;
        for (script, expected) in [
            (
                r#"g.V().values("mixed").trim(Scope.local).toList()"#,
                "The trim(local) step can only take string or list of strings",
            ),
            (
                r#"g.V().values("mixed").lTrim(Scope::local).toList()"#,
                "The lTrim(local) step can only take string or list of strings",
            ),
            (
                r#"g.V().values("nums").asString(Scope.local).toList()"#,
                "Can't parse",
            ),
        ] {
            let graph = empty();
            let err =
                eval_value(Arc::new(graph), &format!("{STRINGS_SETUP}{script}")).expect_err(script);
            assert!(err.to_string().contains(expected), "{script}: {err}");
        }
    }
}

/// `Scope.global` is exactly the unscoped step, including its error on a list.
#[test]
fn string_steps_global_scope_is_the_unscoped_step() {
    let graph = modern();
    let lock = graph.read();
    let names = || lock.traversal().v(None).values("name");
    let pairs = [
        (
            names().to_upper_scoped(Scope::Global).to_list(),
            names().to_upper().to_list(),
        ),
        (
            names().to_lower_scoped(Scope::Global).to_list(),
            names().to_lower().to_list(),
        ),
        (
            names().trim_scoped(Scope::Global).to_list(),
            names().trim().to_list(),
        ),
        (
            names().l_trim_scoped(Scope::Global).to_list(),
            names().l_trim().to_list(),
        ),
        (
            names().r_trim_scoped(Scope::Global).to_list(),
            names().r_trim().to_list(),
        ),
        (
            names().length_scoped(Scope::Global).to_list(),
            names().length().to_list(),
        ),
        (
            names().as_string_scoped(Scope::Global).to_list(),
            names().as_string().to_list(),
        ),
        (
            names().replace_scoped(Scope::Global, "a", "4").to_list(),
            names().replace("a", "4").to_list(),
        ),
        (
            names()
                .substring_scoped(Scope::Global, 1, Some(3))
                .to_list(),
            names().substring(1, 3).to_list(),
        ),
        (
            names().substring_scoped(Scope::Global, 2, None).to_list(),
            names().substring_from(2).to_list(),
        ),
    ];
    for (scoped, unscoped) in pairs {
        assert_eq!(scoped.expect("scoped"), unscoped.expect("unscoped"));
    }
    // Global length of a list is a cast error, like TinkerPop.
    names()
        .fold()
        .length_scoped(Scope::Global)
        .to_list()
        .expect_err("a list is not a string");
    let scoped = names()
        .fold()
        .to_upper_scoped(Scope::Global)
        .to_list()
        .expect_err("a list is not a string");
    let unscoped = names()
        .fold()
        .to_upper()
        .to_list()
        .expect_err("a list is not a string");
    assert_eq!(
        scoped.root_cause().to_string(),
        unscoped.root_cause().to_string()
    );

    #[cfg(feature = "script")]
    {
        use crate::script_host::eval_value;
        use std::sync::Arc;
        let render = |script: &str| {
            let value = eval_value(Arc::new(modern()), script)
                .unwrap_or_else(|err| panic!("{script}: {err}"));
            format!("{value:?}")
        };
        for (scoped, unscoped) in [
            (
                r#"g.V().values("name").toUpper(Scope.global).toList()"#,
                r#"g.V().values("name").toUpper().toList()"#,
            ),
            (
                r#"g.V().values("name").substring(Scope.global, 1, 3).toList()"#,
                r#"g.V().values("name").substring(1, 3).toList()"#,
            ),
            (
                r#"g.V().values("name").replace(Scope::global, "a", "4").toList()"#,
                r#"g.V().values("name").replace("a", "4").toList()"#,
            ),
        ] {
            assert_eq!(render(scoped), render(unscoped), "{scoped}");
        }
    }
}

/// `substring` clamps its indices (TinkerPop's `processStringIndex`): an empty string with a
/// negative index no longer recurses forever, and a multi-byte character is never split.
#[test]
fn substring_clamps_indices() {
    let graph = empty();
    let lock = graph.read();
    for (input, from, to, expected) in [
        ("", -1, Some(2), ""),
        ("", -5, None, ""),
        ("lop", -4, Some(2), "lo"),
        ("ripple", -4, Some(2), ""),
        ("lop", -3, Some(-1), "lo"),
        ("lop", 1, Some(-1), "o"),
        ("lop", 1, Some(0), ""),
        ("lop", 10, None, ""),
        ("héllo", 1, Some(3), "él"),
        ("héllo", -2, None, "lo"),
    ] {
        let values = lock
            .traversal()
            .inject(vec![input])
            .substring_scoped(Scope::Global, from, to)
            .to_list()
            .expect("substring");
        assert_eq!(
            values,
            vec![ElementProperty::from(expected)],
            "{input:?}.substring({from}, {to:?})"
        );
    }
    #[cfg(feature = "script")]
    assert_dsl_rendered(
        empty,
        "",
        r#"g.inject("").substring(-1, 2).toList()"#,
        &[r#""""#],
    );
}

/// `.profile()` names each string step with its scope, and `__.replace()` is no longer shown as
/// `trim`.
#[test]
fn string_steps_profile_names() {
    let graph = modern();
    let lock = graph.read();
    let folded = || lock.traversal().v(None).values("name").fold();
    for (profile, name) in [
        (
            folded().to_upper_scoped(Scope::Local).profile(),
            "to_upper(Scope.local)",
        ),
        (
            folded().l_trim_scoped(Scope::Local).profile(),
            "l_trim(Scope.local)",
        ),
        (
            folded().length_scoped(Scope::Local).profile(),
            "length(Scope.local)",
        ),
        (
            folded().as_string_scoped(Scope::Local).profile(),
            "as_string(Scope.local)",
        ),
        (
            folded().replace_scoped(Scope::Local, "h", "j").profile(),
            "replace(Scope.local, \"h\", \"j\")",
        ),
        (
            folded()
                .substring_scoped(Scope::Local, 1, Some(4))
                .profile(),
            "substring(Scope.local, 1, 4)",
        ),
        (
            folded().substring_scoped(Scope::Local, 2, None).profile(),
            "substring(Scope.local, 2)",
        ),
        (
            lock.traversal()
                .v(None)
                .values("name")
                .to_upper_scoped(Scope::Global)
                .profile(),
            "to_upper()",
        ),
    ] {
        let text = profile.unwrap().to_string();
        assert!(text.contains(name), "{name}: {text}");
        assert!(!text.contains("Optimizer rules applied"), "{text}");
    }
    let text = lock
        .traversal()
        .v(None)
        .values("name")
        .where_t(__::replace("a", "b"))
        .profile()
        .unwrap()
        .to_string();
    assert!(text.contains("\\> replace(\"a\", \"b\")"), "{text}");
    assert!(!text.contains("trim"), "{text}");
}
