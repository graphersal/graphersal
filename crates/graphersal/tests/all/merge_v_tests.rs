//! Integration tests for `mergeV()` with `option(Merge.onCreate|onMatch, ..)`, in the Rust API
//! and in the Rhai DSL (both spellings), plus `inject(map)`.

use std::sync::Arc;

use crate::script_host::{eval_value, rhai::Dynamic};
use graphersal::prelude::*;

/// An empty graph for the DSL.
fn empty() -> Arc<Graph> {
    Arc::new(Graph::new(TraversalGraph::new()))
}

/// The graph of most MergeVertex scenarios: one person marko (29).
fn marko() -> Arc<Graph> {
    let graph = empty();
    let _ = eval_value(
        graph.clone(),
        r#"g.addV("person").property("name", "marko").property("age", 29).toList()"#,
    )
    .unwrap();
    graph
}

/// The script once with `mergeV` and once with `merge_v`, each on a fresh graph from `graph`.
fn both(graph: fn() -> Arc<Graph>, script: &str, check: impl Fn(&Arc<Graph>, Dynamic)) {
    for spelling in [script.to_string(), script.replace("mergeV", "merge_v")] {
        let g = graph();
        let value = eval_value(g.clone(), &spelling)
            .unwrap_or_else(|err| panic!("`{spelling}` failed: {err}"));
        check(&g, value);
    }
}

/// The error of `script`, in both spellings (they must agree).
fn both_err(graph: fn() -> Arc<Graph>, script: &str) -> String {
    let mut messages = Vec::new();
    for spelling in [script.to_string(), script.replace("mergeV", "merge_v")] {
        let err = eval_value(graph(), &spelling)
            .expect_err("script must fail")
            .to_string();
        // The spellings differ in length, and so does the reported script position.
        let message = err
            .rsplit_once(" (line ")
            .map_or(err.as_str(), |(message, _)| message);
        messages.push(message.to_string());
    }
    assert_eq!(messages[0], messages[1]);
    messages.remove(0)
}

fn int(graph: &Arc<Graph>, script: &str) -> i64 {
    eval_value(graph.clone(), script).unwrap().as_int().unwrap()
}

fn len(value: Dynamic) -> usize {
    value.into_array().unwrap().len()
}

fn object(entries: &[(&str, ElementProperty)]) -> ElementProperty {
    let mut map = PropertyMap::default();
    for (key, value) in entries {
        map.insert(*key, value.clone());
    }
    ElementProperty::Object(Box::new(map))
}

fn rust_marko() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "marko")
        .property("age", 29)
        .next()
        .unwrap();
    graph
}

fn count(
    graph: &TraversalGraph,
    key: &str,
    value: impl Into<ElementProperty>,
) -> Option<ElementProperty> {
    graph
        .traversal()
        .v(None)
        .has(key, value)
        .count()
        .next()
        .unwrap()
}

fn one() -> Option<ElementProperty> {
    Some(ElementProperty::from(1))
}

// ---- Rust API -------------------------------------------------------------------------------

#[test]
fn rust_creates_then_matches() {
    let mut graph = rust_marko();
    let stephen = object(&[("T.label", "person".into()), ("name", "stephen".into())]);
    let first = graph
        .traversal_mut()
        .merge_v(stephen.clone())
        .id()
        .next()
        .unwrap();
    let second = graph.traversal_mut().merge_v(stephen).id().next().unwrap();
    assert_eq!(first, second);
    assert_eq!(
        graph.traversal().v(None).count().next().unwrap(),
        Some(ElementProperty::from(2))
    );
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .has_labeled("person", "name", "stephen")
            .count()
            .next()
            .unwrap(),
        one()
    );
}

#[test]
fn rust_on_match_and_on_create() {
    let mut graph = rust_marko();
    graph
        .traversal_mut()
        .merge_v(object(&[
            ("T.label", "person".into()),
            ("name", "marko".into()),
        ]))
        .option_merge(Merge::OnMatch, object(&[("age", 19.into())]))
        .next()
        .unwrap();
    assert_eq!(count(&graph, "age", 19), one());

    graph
        .traversal_mut()
        .merge_v(object(&[
            ("T.label", "person".into()),
            ("name", "stephen".into()),
        ]))
        .option_merge(
            Merge::OnCreate,
            object(&[
                ("T.label", "person".into()),
                ("name", "stephen".into()),
                ("age", 19.into()),
            ]),
        )
        .next()
        .unwrap();
    assert_eq!(
        graph
            .traversal()
            .v(None)
            .has("name", "stephen")
            .has("age", 19)
            .count()
            .next()
            .unwrap(),
        one()
    );
}

#[test]
fn rust_traversal_argument_and_options() {
    let mut graph = rust_marko();
    let created = graph
        .traversal_mut()
        .with_side_effect(
            "c",
            object(&[("T.label", "person".into()), ("name", "stephen".into())]),
        )
        .with_side_effect("m", object(&[("age", 19.into())]))
        .merge_v_by(__::select("c"))
        .option_merge_by(Merge::OnCreate, __::select("m"))
        .values("age")
        .to_list()
        .unwrap();
    assert_eq!(created, vec![ElementProperty::from(19)]);
}

#[test]
fn rust_option_with_cardinality() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .merge_v(object(&[("name", "alice".into())]))
        .option_merge_with(
            Merge::OnCreate,
            object(&[("age", 81.into())]),
            Cardinality::Single,
        )
        .next()
        .unwrap();
    assert_eq!(count(&graph, "age", 81), one());

    let err = graph
        .traversal_mut()
        .merge_v(object(&[("name", "bob".into())]))
        .option_merge_with(
            Merge::OnCreate,
            object(&[("age", 81.into())]),
            Cardinality::List,
        )
        .next()
        .unwrap_err();
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::UnsupportedCardinality { .. }
        ),
        "{err}"
    );
    assert_eq!(count(&graph, "name", "bob"), Some(ElementProperty::from(0)));
}

#[test]
fn rust_option_out_v_on_merge_v_and_option_without_merge_fail() {
    let mut graph = TraversalGraph::new();
    let err = graph
        .traversal_mut()
        .merge_v(ElementProperty::Null)
        .option_merge(Merge::OutV, ElementProperty::Null)
        .next()
        .unwrap_err();
    assert!(
        matches!(err.root_cause(), TraverserError::InvalidMergeMap { .. }),
        "{err}"
    );

    let err = graph
        .traversal_mut()
        .inject(vec![1])
        .option_merge(Merge::OnCreate, ElementProperty::Null)
        .next()
        .unwrap_err();
    assert!(
        matches!(err.root_cause(), TraverserError::OptionWithoutBranch { .. }),
        "{err}"
    );
    assert!(
        err.root_cause().to_string().contains("merge_v()/merge_e()"),
        "{err}"
    );
}

#[test]
fn rust_second_on_create_replaces_the_first() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .merge_v(object(&[("name", "x".into())]))
        .option_merge(Merge::OnCreate, object(&[("age", 1.into())]))
        .option_merge(Merge::OnCreate, object(&[("age", 2.into())]))
        .next()
        .unwrap();
    assert_eq!(count(&graph, "age", 2), one());
    assert_eq!(count(&graph, "age", 1), Some(ElementProperty::from(0)));
}

#[test]
fn rust_anonymous_merge_v_runs_per_traverser() {
    let mut graph = rust_marko();
    let results = graph
        .traversal_mut()
        .inject(vec![1, 2])
        .local(__::merge_v(object(&[("name", "josh".into())])))
        .to_list()
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(count(&graph, "name", "josh"), one());
}

#[test]
fn rust_count_still_mutates() {
    let mut graph = TraversalGraph::new();
    let counted = graph
        .traversal_mut()
        .merge_v(object(&[("name", "x".into())]))
        .count()
        .next()
        .unwrap();
    assert_eq!(counted, one());
    assert_eq!(count(&graph, "name", "x"), one());
}

#[test]
fn rust_schema_enforcement_applies_to_created_vertices() {
    let mut graph = rust_marko();
    let schema = graph.infer_schema().unwrap().with_mode(SchemaMode::Closed);
    graph.set_schema(schema).unwrap();
    let err = graph
        .traversal_mut()
        .merge_v(object(&[
            ("T.label", "person".into()),
            ("name", "x".into()),
            ("color", "red".into()),
        ]))
        .next()
        .unwrap_err();
    assert!(err.to_string().contains("color"), "{err}");
    assert_eq!(count(&graph, "name", "x"), Some(ElementProperty::from(0)));
}

// ---- DSL ------------------------------------------------------------------------------------

#[test]
fn dsl_creates_then_matches_the_same_vertex() {
    both(
        marko,
        r#"g.mergeV(#{"T.label": "person", "name": "stephen"}).id().next()"#,
        |g, first| {
            let again = eval_value(
                g.clone(),
                r#"g.mergeV(#{"T.label": "person", "name": "stephen"}).id().next()"#,
            )
            .unwrap();
            assert_eq!(first.to_string(), again.to_string());
            assert_eq!(int(g, "g.v().count().next()"), 2);
        },
    );
}

#[test]
fn dsl_on_match_sets_properties() {
    both(
        marko,
        r#"g.mergeV(#{"T.label": "person", "name": "marko"}).option(Merge.onMatch, #{"age": 19}).toList()"#,
        |g, value| {
            assert_eq!(len(value), 1);
            assert_eq!(
                int(
                    g,
                    r#"g.v().has("person", "name", "marko").has("age", 19).count().next()"#
                ),
                1
            );
        },
    );
}

#[test]
fn dsl_on_create_with_inherited_keys() {
    both(
        marko,
        r#"g.mergeV(#{"T.label": "person", "name": "stephen"}).option(Merge.onCreate, #{"T.label": "person", "name": "stephen", "age": 19}).toList()"#,
        |g, _| {
            assert_eq!(
                int(
                    g,
                    r#"g.v().has("person", "name", "stephen").has("age", 19).count().next()"#
                ),
                1
            )
        },
    );
    both(
        empty,
        r#"g.mergeV(#{"T.id": "1"}).option(Merge.onCreate, #{"T.label": "person", "name": "mike"}).toList()"#,
        |g, _| {
            assert_eq!(
                int(
                    g,
                    r#"g.v("1").has("person", "name", "mike").count().next()"#
                ),
                1
            )
        },
    );
}

#[test]
fn dsl_empty_and_null_maps_match_everything() {
    both(marko, "g.mergeV(#{}).toList()", |g, value| {
        assert_eq!(len(value), 1);
        assert_eq!(int(g, "g.v().count().next()"), 1);
    });
    both(marko, "g.mergeV(()).toList()", |_, value| {
        assert_eq!(len(value), 1)
    });
    let two = || {
        let g = marko();
        let _ = eval_value(
            g.clone(),
            r#"g.addV("person").property("name", "vadas").toList()"#,
        )
        .unwrap();
        g
    };
    both(two, "g.V().mergeV(#{}).toList()", |g, value| {
        assert_eq!(len(value), 4);
        assert_eq!(int(g, "g.v().count().next()"), 2);
    });
    both(empty, "g.mergeV(#{}).toList()", |g, value| {
        assert_eq!(len(value), 1);
        assert_eq!(int(g, "g.v().count().next()"), 1);
    });
}

#[test]
fn dsl_mid_traversal_runs_per_traverser() {
    both(
        marko,
        r#"g.inject(0).mergeV(#{"T.label": "person", "name": "stephen"}).toList()"#,
        |g, value| {
            assert_eq!(len(value), 1);
            assert_eq!(
                int(
                    g,
                    r#"g.v().has("person", "name", "stephen").count().next()"#
                ),
                1
            );
        },
    );
    // An empty stream merges nothing mid-traversal.
    both(
        marko,
        r#"g.V().has("name", "nobody").mergeV(#{"name": "x"}).toList()"#,
        |g, value| {
            assert_eq!(len(value), 0);
            assert_eq!(int(g, "g.v().count().next()"), 1);
        },
    );
}

#[test]
fn dsl_merge_v_without_argument_merges_injected_maps() {
    both(
        marko,
        r#"g.inject(#{"T.label": "person", "name": "marko"}, #{"T.label": "person", "name": "stephen"}).mergeV().toList()"#,
        |g, value| {
            assert_eq!(len(value), 2);
            assert_eq!(int(g, "g.v().count().next()"), 2);
        },
    );
    both(
        marko,
        r#"g.inject(#{"T.label": "person", "name": "marko"}).mergeV(__.identity()).toList()"#,
        |g, _| assert_eq!(int(g, "g.v().count().next()"), 1),
    );
}

#[test]
fn dsl_traversal_argument_and_options_see_side_effects() {
    both(
        marko,
        r#"g.withSideEffect("c", #{"T.label": "person", "name": "stephen"}).withSideEffect("m", #{"T.label": "person", "name": "stephen", "age": 19}).mergeV(__.select("c")).option(Merge.onCreate, __.select("m")).toList()"#,
        |g, _| {
            assert_eq!(
                int(
                    g,
                    r#"g.v().has("person", "name", "stephen").has("age", 19).count().next()"#
                ),
                1
            )
        },
    );
    both(
        marko,
        r#"g.withSideEffect("c", #{"T.label": "person", "name": "marko"}).withSideEffect("m", #{"age": 19}).inject(0).mergeV(__.select("c")).option(Merge.onMatch, __.select("m")).toList()"#,
        |g, _| {
            assert_eq!(
                int(
                    g,
                    r#"g.v().has("person", "name", "marko").has("age", 19).count().next()"#
                ),
                1
            )
        },
    );
}

#[test]
fn dsl_on_match_traversal_receives_the_matched_vertex() {
    let two = || {
        let g = marko();
        let _ = eval_value(
            g.clone(),
            r#"g.addV("person").property("name", "vadas").property("age", 27).toList()"#,
        )
        .unwrap();
        g
    };
    // Every match is updated; the onMatch traversal sees each matched vertex.
    both(
        two,
        r#"g.mergeV(#{"T.label": "person"}).option(Merge.onMatch, __.project("previous").by("age")).toList()"#,
        |g, value| {
            assert_eq!(len(value), 2);
            assert_eq!(
                int(g, r#"g.v().has("name", "marko").values("previous").next()"#),
                29
            );
            assert_eq!(
                int(g, r#"g.v().has("name", "vadas").values("previous").next()"#),
                27
            );
        },
    );
}

#[test]
fn dsl_cardinality_values() {
    both(
        empty,
        r#"g.mergeV(#{"name": "alice", "T.label": "person"}).option(Merge.onCreate, #{"age": Cardinality.single(81)}).toList()"#,
        |g, _| {
            assert_eq!(
                int(
                    g,
                    r#"g.v().has("person", "name", "alice").has("age", 81).count().next()"#
                ),
                1
            )
        },
    );
    both(
        empty,
        r#"g.mergeV(#{"name": "alice"}).option(Merge.onCreate, #{"age": 81}, Cardinality.single).toList()"#,
        |g, _| assert_eq!(int(g, r#"g.v().has("age", 81).count().next()"#), 1),
    );
    let err = both_err(
        empty,
        r#"g.mergeV(#{"name": "alice", "T.label": "person"}).option(Merge.onCreate, #{"age": Cardinality.set(81)}).toList()"#,
    );
    assert!(err.contains("Cardinality.set is not supported"), "{err}");
}

#[test]
fn dsl_errors() {
    let err = both_err(
        empty,
        r#"g.mergeV(#{"T.label": "a"}).option(Merge.onCreate, #{"T.label": "b"}).toList()"#,
    );
    assert!(
        err.contains("option(onCreate) cannot override values from merge() argument"),
        "{err}"
    );
    let err = both_err(empty, r#"g.mergeV(#{"~id": "1"}).toList()"#);
    assert!(
        err.contains("Property key can not be a hidden key: ~id"),
        "{err}"
    );
    let err = both_err(
        empty,
        r#"g.mergeV(#{}).option(Merge.onCreate, #{"~label": "vertex"}).toList()"#,
    );
    assert!(
        err.contains("Property key can not be a hidden key: ~label"),
        "{err}"
    );
    let err = both_err(empty, r#"g.mergeV(#{"T.label": "~vertex"}).toList()"#);
    assert!(
        err.contains("Label can not be a hidden key: ~vertex"),
        "{err}"
    );
    let err = both_err(empty, r#"g.mergeV(#{"T.label": ()}).toList()"#);
    assert!(err.contains("T.label must be a non-empty string"), "{err}");
    let err = both_err(empty, r#"g.mergeV(#{"T.key": "x"}).toList()"#);
    assert!(err.contains("unsupported token key \"T.key\""), "{err}");
    let err = both_err(empty, r#"g.mergeV(#{"Direction.OUT": "1"}).toList()"#);
    assert!(
        err.contains("merge_v() does not accept Direction keys"),
        "{err}"
    );
    let err = both_err(
        marko,
        r#"g.mergeV(#{}).option(Merge.onMatch, #{"T.label": "x"}).toList()"#,
    );
    assert!(
        err.contains("option(onMatch) cannot change T.id, T.label"),
        "{err}"
    );
    let err = both_err(empty, "g.mergeV(5).toList()");
    assert!(err.contains("merge_v() expects a map, got int64"), "{err}");
    let err = both_err(empty, "g.inject(1).mergeV(__.limit(0)).toList()");
    assert!(err.contains("merge_v() traversal produced no map"), "{err}");
}

#[test]
fn dsl_dynamic_override_fails_only_on_create() {
    let err = both_err(
        empty,
        r#"g.withSideEffect("s", #{"T.label": "b"}).mergeV(#{"T.label": "a"}).option(Merge.onCreate, __.select("s")).toList()"#,
    );
    assert!(
        err.contains("option(onCreate) cannot override values from merge() argument"),
        "{err}"
    );
    // With a match the onCreate traversal never runs.
    let with_a = || {
        let g = empty();
        let _ = eval_value(g.clone(), r#"g.addV("a").toList()"#).unwrap();
        g
    };
    both(
        with_a,
        r#"g.withSideEffect("s", #{"T.label": "b"}).mergeV(#{"T.label": "a"}).option(Merge.onCreate, __.select("s")).toList()"#,
        |_, value| assert_eq!(len(value), 1),
    );
}

#[test]
fn dsl_invalid_constant_map_fails_before_execution() {
    // No traverser reaches the step, the problem is still reported.
    let err = both_err(
        marko,
        r#"g.V().has("name", "nobody").mergeV(#{"~id": 1}).toList()"#,
    );
    assert!(
        err.contains("Property key can not be a hidden key: ~id"),
        "{err}"
    );
    // Nested in a child traversal that never runs.
    let err = both_err(
        marko,
        r#"g.V().has("name", "nobody").local(__.mergeV(#{}).option(Merge.onMatch, #{"~id": 1})).toList()"#,
    );
    assert!(
        err.contains("Property key can not be a hidden key: ~id"),
        "{err}"
    );
    // An empty graph with onMatch: the map is never used, still rejected.
    let err = both_err(
        empty,
        r#"g.mergeV(#{}).option(Merge.onMatch, #{"~label": "vertex"}).toList()"#,
    );
    assert!(
        err.contains("Property key can not be a hidden key: ~label"),
        "{err}"
    );
}

#[test]
fn dsl_option_without_merge_step() {
    let err = eval_value(
        empty(),
        r#"g.inject(1).option(Merge.onCreate, #{}).toList()"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("merge_v()/merge_e()"), "{err}");
}

#[test]
fn dsl_profile_shows_the_lookup() {
    let g = marko();
    let profile = eval_value(
        g.clone(),
        r#"g.mergeV(#{"T.label": "person"}).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(
        profile.contains(r#"merge_v(#{"T.label": "person"}) [lookup: label]"#),
        "{profile}"
    );
    let profile = eval_value(
        g.clone(),
        r#"g.mergeV(#{"T.id": "1"}).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(profile.contains("[lookup: id]"), "{profile}");
    let profile = eval_value(g.clone(), r#"g.mergeV(#{"age": 29}).profile().to_string()"#)
        .unwrap()
        .to_string();
    assert!(profile.contains("[lookup: scan]"), "{profile}");
    let profile = eval_value(g.clone(), "g.inject(#{}).mergeV().profile().to_string()")
        .unwrap()
        .to_string();
    assert!(profile.contains("[lookup: dynamic]"), "{profile}");
}

#[test]
fn dsl_count_still_mutates() {
    both(
        empty,
        r#"g.mergeV(#{"name": "x"}).count().next()"#,
        |g, value| {
            assert_eq!(value.as_int().unwrap(), 1);
            assert_eq!(int(g, r#"g.v().has("name", "x").count().next()"#), 1);
        },
    );
}

#[test]
fn inject_map_is_one_value() {
    let g = empty();
    let value = eval_value(g, r#"g.inject(#{"a": 1}, #{"b": 2}).toList()"#).unwrap();
    assert_eq!(len(value), 2);
    let mut graph = TraversalGraph::new();
    let values = graph
        .traversal_mut()
        .inject(vec![object(&[("a", 1.into())])])
        .to_list()
        .unwrap();
    assert_eq!(values, vec![object(&[("a", 1.into())])]);
}

#[test]
fn dsl_project_record_is_a_merge_map() {
    both(
        empty,
        r#"g.inject(1).project("name", "T.label").by(__.constant("q")).by(__.constant("person")).mergeV().toList()"#,
        |g, _| {
            assert_eq!(
                int(g, r#"g.v().has("person", "name", "q").count().next()"#),
                1
            )
        },
    );
}
