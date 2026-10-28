//! `has_label()` without arguments (a Graphersal extension): keeps the elements that have a label;
//! `not(__.has_label())` selects the unlabeled vertices and edges.

#[cfg(feature = "script")]
use std::sync::Arc;

#[cfg(feature = "script")]
use crate::script_host::eval_value;
use graphersal::prelude::*;

/// Two labeled vertices `a` (`["a"]`) and `ab` (`["a", "b"]`), one unlabeled vertex `bare`, a
/// labeled edge `e1` (`a -> ab`) and an unlabeled edge `e2` (`a -> bare`).
fn mixed_graph() -> Graph {
    let mut inner = TraversalGraph::new();
    let a = inner
        .add_vertex(Some("a".into()), vec!["a".into()], Default::default())
        .unwrap();
    let ab = inner
        .add_vertex(
            Some("ab".into()),
            vec!["a".into(), "b".into()],
            Default::default(),
        )
        .unwrap();
    let bare = inner
        .add_vertex(Some("bare".into()), vec![], Default::default())
        .unwrap();
    inner
        .add_edge(
            Some("e1".into()),
            Some("knows".into()),
            a,
            ab,
            Default::default(),
        )
        .unwrap();
    inner
        .add_edge(Some("e2".into()), None, a, bare, Default::default())
        .unwrap();
    Graph::new(inner)
}

#[cfg(feature = "script")]
fn text(graph: &Arc<Graph>, script: &str) -> String {
    eval_value(graph.clone(), script)
        .unwrap_or_else(|err| panic!("{script}: {err}"))
        .to_string()
}

#[cfg(feature = "script")]
#[test]
fn dsl_selects_labeled_and_unlabeled_elements_in_both_spellings() {
    let graph = Arc::new(mixed_graph());
    for (script, expected) in [
        (r#"g.v().has_label().id().to_list()"#, r#"["a", "ab"]"#),
        (r#"g.V().hasLabel().id().toList()"#, r#"["a", "ab"]"#),
        (r#"g.v().not(__.has_label()).id().to_list()"#, r#"["bare"]"#),
        (r#"g.V().not(__.hasLabel()).id().toList()"#, r#"["bare"]"#),
        (r#"g.e().has_label().id().to_list()"#, r#"["e1"]"#),
        (r#"g.E().hasLabel().id().toList()"#, r#"["e1"]"#),
        (r#"g.e().not(__.has_label()).id().to_list()"#, r#"["e2"]"#),
        (r#"g.v().where(__.has_label()).count().next()"#, "2"),
        (
            r#"g.v("a").out_e().has_label().id().to_list()"#,
            r#"["e1"]"#,
        ),
        (
            r#"g.v("a").out_e().not(__.has_label()).in_v().id().to_list()"#,
            r#"["bare"]"#,
        ),
        // An empty list and null still keep nothing.
        (r#"g.v().has_label([]).count().next()"#, "0"),
        (r#"g.v().has_label(()).count().next()"#, "0"),
    ] {
        assert_eq!(text(&graph, script), expected, "{script}");
    }
}

#[cfg(feature = "script")]
#[test]
fn vertex_properties_always_have_a_label() {
    let graph = Arc::new(GraphSource::tinkerpop_modern());
    assert_eq!(
        text(&graph, r#"g.v(1).properties().has_label().count().next()"#),
        "2"
    );
}

#[test]
fn rust_api_has_label_none() {
    let graph = mixed_graph();
    let lock = graph.read();
    let ids = |list: Vec<ElementProperty>| -> Vec<String> {
        list.into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let labeled = lock
        .traversal()
        .v(None::<()>)
        .has_label(None::<()>)
        .id()
        .to_list()
        .unwrap();
    assert_eq!(ids(labeled), ["a", "ab"]);
    let unlabeled = lock
        .traversal()
        .v(None::<()>)
        .not(__::has_label(None::<()>))
        .id()
        .to_list()
        .unwrap();
    assert_eq!(ids(unlabeled), ["bare"]);
    let edges = lock
        .traversal()
        .e(None::<()>)
        .not(__::has_label(None::<()>))
        .id()
        .to_list()
        .unwrap();
    assert_eq!(ids(edges), ["e2"]);
}

/// The optimizer must not fold `has_label()` into the source step as an empty label list (which
/// would keep nothing), nor into an edge step; the profile shows the step as `has_label()`.
#[cfg(feature = "script")]
#[test]
fn profile_shows_has_label_and_nothing_is_folded() {
    let graph = Arc::new(mixed_graph());
    let profile = text(&graph, r#"g.v().has_label().count().profile().to_string()"#);
    assert!(profile.contains("has_label()"), "{profile}");
    assert!(!profile.contains("labels:"), "{profile}");
    let profile = text(
        &graph,
        r#"g.v("a").out_e().has_label().count().profile().to_string()"#,
    );
    assert!(profile.contains("has_label()"), "{profile}");
    assert!(!profile.contains("labels:"), "{profile}");
    // The same traversals return the labeled elements, not nothing.
    assert_eq!(text(&graph, r#"g.v().has_label().count().next()"#), "2");
    assert_eq!(
        text(&graph, r#"g.v().has_label().has_label("b").id().to_list()"#),
        r#"["ab"]"#
    );
    assert_eq!(
        text(&graph, r#"g.v().has_label("b").has_label().id().to_list()"#),
        r#"["ab"]"#
    );
    assert_eq!(
        text(&graph, r#"g.v("a").out_e().has_label().count().next()"#),
        "1"
    );
    // Camel spelling of the profile.
    let camel = text(
        &graph,
        r#"g.with("render.spelling", "camel").v().has_label().count().profile().to_string()"#,
    );
    assert!(camel.contains("hasLabel()"), "{camel}");
}

/// The freeze recipe of the closed-schema help: label every unlabeled element, then freeze.
#[cfg(feature = "script")]
#[test]
fn freeze_recipe_end_to_end() {
    let graph = Arc::new(mixed_graph());
    let err = eval_value(
        graph.clone(),
        r#"g.set_schema(g.infer_schema(), SchemaMode.closed)"#,
    )
    .expect_err("unlabeled elements block a closed freeze")
    .to_string();
    assert!(err.contains("g.v().not(__.has_label())"), "{err}");
    let fix = r#"
        g.v().not(__.has_label()).add_label("thing").to_list();
        g.e().not(__.has_label()).set_label("link").to_list();
        g.set_schema(g.infer_schema(), SchemaMode.closed);
        [g.get_schema().mode, g.v().not(__.has_label()).count().next(), g.e("e2").label().next()]
    "#;
    assert_eq!(text(&graph, fix), r#"["closed", 0, "link"]"#);
}

fn count(list: Vec<ElementProperty>) -> i64 {
    list[0].as_i64().expect("a count")
}

/// A value converts to a step argument, never to "no argument": Rust
/// `has_label(ElementProperty::Null)` keeps nothing on vertices and edges (like the DSL's
/// `has_label(null)`), while `has_label(None::<()>)` keeps the labelled ones.
#[cfg(feature = "script")]
#[test]
fn rust_api_has_label_null_keeps_nothing() {
    let graph = mixed_graph();
    let lock = graph.read();
    let g = || lock.traversal();
    let null = || ElementProperty::Null;
    let none = || None::<()>;
    assert_eq!(
        count(g().v(none()).has_label(null()).count().to_list().unwrap()),
        0
    );
    assert_eq!(
        count(g().e(none()).has_label(null()).count().to_list().unwrap()),
        0
    );
    assert_eq!(
        count(
            g().v(none())
                .has_label(&ElementProperty::Null)
                .count()
                .to_list()
                .unwrap()
        ),
        0
    );
    assert_eq!(
        count(
            g().v(none())
                .not(__::has_label(null()))
                .count()
                .to_list()
                .unwrap()
        ),
        3
    );
    assert_eq!(
        count(g().v(none()).has_label(none()).count().to_list().unwrap()),
        2
    );
    assert_eq!(
        count(g().e(none()).has_label(none()).count().to_list().unwrap()),
        1
    );
    drop(lock);
    // The DSL agrees (Rhai spells null `()`).
    let shared = Arc::new(graph);
    assert_eq!(text(&shared, "g.v().has_label(()).count().next()"), "0");
    assert_eq!(text(&shared, "g.E().hasLabel(()).count().next()"), "0");
}

/// The other users of the value conversion: a null or a map names nothing, so it matches nothing,
/// where `None::<()>` keeps its "no argument" meaning (every element, label, key).
#[test]
fn rust_api_null_and_map_arguments_match_nothing_in_every_step() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.read();
    let g = || lock.traversal();
    let none = || None::<()>;
    // (step, with no argument)
    let no_argument = [
        ("v", count(g().v(none()).count().to_list().unwrap())),
        ("e", count(g().e(none()).count().to_list().unwrap())),
        (
            "out",
            count(g().v(none()).out(none()).count().to_list().unwrap()),
        ),
        (
            "in_e",
            count(g().v(none()).in_e(none()).count().to_list().unwrap()),
        ),
        (
            "both",
            count(g().v(none()).both(none()).count().to_list().unwrap()),
        ),
        (
            "values",
            count(g().v(none()).values(none()).count().to_list().unwrap()),
        ),
        (
            "properties",
            count(g().v(none()).properties(none()).count().to_list().unwrap()),
        ),
    ];
    assert_eq!(
        no_argument.map(|(_, n)| n),
        [6, 6, 6, 6, 12, 12, 12],
        "{no_argument:?}"
    );
    for (name, value) in [
        ("null", ElementProperty::Null),
        ("map", ElementProperty::Map(Default::default())),
    ] {
        let a = || value.clone();
        let with_value = [
            ("v", count(g().v(a()).count().to_list().unwrap())),
            ("e", count(g().e(a()).count().to_list().unwrap())),
            (
                "out",
                count(g().v(none()).out(a()).count().to_list().unwrap()),
            ),
            (
                "in_e",
                count(g().v(none()).in_e(a()).count().to_list().unwrap()),
            ),
            (
                "both",
                count(g().v(none()).both(a()).count().to_list().unwrap()),
            ),
            (
                "values",
                count(g().v(none()).values(a()).count().to_list().unwrap()),
            ),
            (
                "properties",
                count(g().v(none()).properties(a()).count().to_list().unwrap()),
            ),
        ];
        for (step, n) in with_value {
            assert_eq!(n, 0, "{step}({name})");
        }
    }
}

/// `remove_property(null)` removes nothing (an empty key list), where `remove_property(None)` is
/// the rejected no-key form.
#[test]
fn rust_api_remove_property_null_removes_nothing() {
    let graph = GraphSource::tinkerpop_modern();
    let lock = graph.write();
    let remaining = lock
        .traversal()
        .v("1")
        .remove_property(ElementProperty::Null)
        .values(None::<()>)
        .count()
        .to_list()
        .unwrap();
    assert_eq!(count(remaining), 2);
    assert!(
        lock.traversal()
            .v("1")
            .remove_property(None::<()>)
            .to_list()
            .is_err()
    );
}
