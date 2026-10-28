//! Integration tests for `mergeE()` with `option(Merge.onCreate|onMatch|outV|inV, ..)`, in the
//! Rust API and in the Rhai DSL (both spellings).

#[cfg(feature = "script")]
use std::sync::Arc;

#[cfg(feature = "script")]
use crate::script_host::{eval_value, rhai::Dynamic};
use graphersal::prelude::*;

/// An empty graph for the DSL.
#[cfg(feature = "script")]
fn empty() -> Arc<Graph> {
    Arc::new(Graph::new(TraversalGraph::new()))
}

/// Two persons: marko (id "1") and vadas (id "2"), no edges.
#[cfg(feature = "script")]
fn two() -> Arc<Graph> {
    let graph = empty();
    let _ = eval_value(
        graph.clone(),
        r#"g.addV("person").property("name", "marko").addV("person").property("name", "vadas").toList()"#,
    )
    .unwrap();
    graph
}

/// [`two`] plus two `knows` edges from marko to vadas, the first with `created: "Y"`.
#[cfg(feature = "script")]
fn two_knows() -> Arc<Graph> {
    let graph = two();
    let _ = eval_value(
        graph.clone(),
        r#"g.V("1").addE("knows").to("2").property("created", "Y").V("1").addE("knows").to("2").toList()"#,
    )
    .unwrap();
    graph
}

/// The script once with `mergeE` and once with `merge_e`, each on a fresh graph from `graph`.
#[cfg(feature = "script")]
fn both(graph: fn() -> Arc<Graph>, script: &str, check: impl Fn(&Arc<Graph>, Dynamic)) {
    for spelling in [script.to_string(), script.replace("mergeE", "merge_e")] {
        let g = graph();
        let value = eval_value(g.clone(), &spelling)
            .unwrap_or_else(|err| panic!("`{spelling}` failed: {err}"));
        check(&g, value);
    }
}

/// The error of `script`, in both spellings (they must agree).
#[cfg(feature = "script")]
fn both_err(graph: fn() -> Arc<Graph>, script: &str) -> String {
    let mut messages = Vec::new();
    for spelling in [script.to_string(), script.replace("mergeE", "merge_e")] {
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

#[cfg(feature = "script")]
fn int(graph: &Arc<Graph>, script: &str) -> i64 {
    eval_value(graph.clone(), script).unwrap().as_int().unwrap()
}

#[cfg(feature = "script")]
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

/// `{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2"}`.
fn knows_1_2() -> ElementProperty {
    object(&[
        ("T.label", "knows".into()),
        ("Direction.OUT", "1".into()),
        ("Direction.IN", "2".into()),
    ])
}

/// The Rust counterpart of [`two`].
fn rust_two() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .add_v("person")
        .property("name", "marko")
        .add_v("person")
        .property("name", "vadas")
        .next()
        .unwrap();
    graph
}

fn edges(graph: &TraversalGraph) -> Option<ElementProperty> {
    graph.traversal().e(None).count().next().unwrap()
}

fn n(value: i64) -> Option<ElementProperty> {
    Some(ElementProperty::from(value))
}

// ---- Rust API -------------------------------------------------------------------------------

#[test]
fn rust_creates_then_matches() {
    let mut graph = rust_two();
    let first = graph
        .traversal_mut()
        .merge_e(knows_1_2())
        .id()
        .next()
        .unwrap();
    let second = graph
        .traversal_mut()
        .merge_e(knows_1_2())
        .id()
        .next()
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(edges(&graph), n(1));
    assert_eq!(
        graph.traversal().v("1").out("knows").id().next().unwrap(),
        Some(ElementProperty::from("2"))
    );
}

#[test]
fn rust_out_v_in_v_options_map_and_traversal() {
    let mut graph = rust_two();
    let map = object(&[
        ("T.label", "knows".into()),
        ("Direction.OUT", "Merge.outV".into()),
        ("Direction.IN", "Merge.inV".into()),
    ]);
    graph
        .traversal_mut()
        .merge_e(map.clone())
        .option_merge(Merge::OutV, object(&[("name", "marko".into())]))
        .option_merge(Merge::InV, object(&[("T.id", "2".into())]))
        .next()
        .unwrap();
    assert_eq!(edges(&graph), n(1));
    // The same edge through select() options: matched, not created again.
    let found = graph
        .traversal_mut()
        .v("1")
        .as_("x")
        .v("2")
        .as_("y")
        .merge_e(map)
        .option_merge_by(Merge::OutV, __::select("x"))
        .option_merge_by(Merge::InV, __::select("y"))
        .to_list()
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(edges(&graph), n(1));
}

#[test]
fn rust_on_create_and_on_match() {
    let mut graph = rust_two();
    let created = || object(&[("created", "Y".into())]);
    let matched = || object(&[("created", "N".into())]);
    for _ in 0..2 {
        graph
            .traversal_mut()
            .merge_e(knows_1_2())
            .option_merge(Merge::OnCreate, created())
            .option_merge(Merge::OnMatch, matched())
            .next()
            .unwrap();
    }
    assert_eq!(edges(&graph), n(1));
    assert_eq!(
        graph.traversal().e(None).values("created").next().unwrap(),
        Some(ElementProperty::from("N"))
    );
}

#[test]
fn rust_errors() {
    let mut graph = rust_two();
    let err = graph
        .traversal_mut()
        .merge_e(object(&[]))
        .next()
        .unwrap_err();
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::MergeVertexUnresolved {
                direction: Direction::Out,
                ..
            }
        ),
        "{err}"
    );
    assert!(
        err.root_cause()
            .to_string()
            .contains("Out Vertex not specified"),
        "{err}"
    );
    let err = graph
        .traversal_mut()
        .merge_e(object(&[("Direction.OUT", "1".into())]))
        .next()
        .unwrap_err();
    assert!(
        err.root_cause()
            .to_string()
            .contains("In Vertex not specified"),
        "{err}"
    );
    // Cardinality on an edge map is rejected before execution.
    let err = graph
        .traversal_mut()
        .merge_e(knows_1_2())
        .option_merge_with(
            Merge::OnMatch,
            object(&[("w", 1.into())]),
            Cardinality::Single,
        )
        .next()
        .unwrap_err();
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::UnsupportedCardinality { on_edge: true, .. }
        ),
        "{err}"
    );
    assert_eq!(edges(&graph), n(0));
}

#[test]
fn rust_anonymous_merge_e_runs_per_traverser() {
    let mut graph = rust_two();
    let self_loop = object(&[
        ("T.label", "self".into()),
        ("Direction.OUT", "Merge.outV".into()),
        ("Direction.IN", "Merge.inV".into()),
    ]);
    let made = graph
        .traversal_mut()
        .v(None)
        .as_("v")
        .local(
            __::merge_e(self_loop)
                .option_merge_by(Merge::OutV, __::select("v"))
                .option_merge_by(Merge::InV, __::select("v")),
        )
        .to_list()
        .unwrap();
    assert_eq!(made.len(), 2);
    assert_eq!(edges(&graph), n(2));
    let by = graph
        .traversal_mut()
        .merge_e_by(__::constant(knows_1_2()))
        .to_list()
        .unwrap();
    assert_eq!(by.len(), 1);
    assert_eq!(edges(&graph), n(3));
}

#[test]
fn rust_count_still_mutates() {
    let mut graph = rust_two();
    let counted = graph
        .traversal_mut()
        .merge_e(knows_1_2())
        .count()
        .next()
        .unwrap();
    assert_eq!(counted, n(1));
    assert_eq!(edges(&graph), n(1));
}

#[test]
fn rust_schema_enforcement_applies_to_created_edges() {
    let mut graph = rust_two();
    graph.traversal_mut().merge_e(knows_1_2()).next().unwrap();
    let schema = graph.infer_schema().unwrap().with_mode(SchemaMode::Closed);
    graph.set_schema(schema).unwrap();
    let err = graph
        .traversal_mut()
        .merge_e(object(&[
            ("T.label", "knows".into()),
            ("Direction.OUT", "2".into()),
            ("Direction.IN", "1".into()),
            ("color", "red".into()),
        ]))
        .next()
        .unwrap_err();
    assert!(err.to_string().contains("color"), "{err}");
    assert_eq!(edges(&graph), n(1));
}

// ---- DSL ------------------------------------------------------------------------------------

#[cfg(feature = "script")]
#[test]
fn dsl_creates_then_matches_the_same_edge() {
    both(
        two,
        r#"let a = g.mergeE(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2"}).id().next();
           let b = g.mergeE(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2"}).id().next();
           a == b"#,
        |g, same| {
            assert!(same.as_bool().unwrap());
            assert_eq!(int(g, "g.E().count().next()"), 1);
            assert_eq!(int(g, r#"g.V("1").out("knows").count().next()"#), 1);
        },
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_out_v_in_v_options() {
    // select() options, map options, and the Merge::outV spelling all find the same edge.
    for script in [
        r#"g.V("1").as("x").V("2").as("y").mergeE(#{"T.label": "knows", "Direction.OUT": Merge.outV, "Direction.IN": Merge.inV}).option(Merge.outV, __.select("x")).option(Merge.inV, __.select("y")).toList()"#,
        r#"g.mergeE(#{"Direction.OUT": Merge.outV, "Direction.IN": Merge.inV, "T.label": "knows"}).option(Merge.outV, #{"T.id": "1"}).option(Merge.inV, #{"T.id": "2"}).toList()"#,
        r#"g.mergeE(#{"Direction.OUT": Merge::outV, "Direction.IN": Merge::inV, "T.label": "knows"}).option(Merge::outV, #{"name": "marko"}).option(Merge::inV, #{"name": "vadas"}).toList()"#,
    ] {
        both(two, script, |g, value| {
            assert_eq!(len(value), 1);
            assert_eq!(
                int(
                    g,
                    r#"g.V("1").out("knows").has("name", "vadas").count().next()"#
                ),
                1
            );
        });
    }
    both(
        two,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2"}).toList();
           g.V("1").as("x").V("2").as("y").mergeE(#{"T.label": "knows", "Direction.OUT": Merge.outV, "Direction.IN": Merge.inV}).option(Merge.outV, __.select("x")).option(Merge.inV, __.select("y")).toList()"#,
        |g, value| {
            assert_eq!(len(value), 1);
            assert_eq!(int(g, "g.E().count().next()"), 1);
        },
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_one_self_loop_per_vertex() {
    let script = r#"g.V().as("v").mergeE(#{"T.label": "self", "Direction.OUT": Merge.outV, "Direction.IN": Merge.inV}).option(Merge.outV, __.select("v")).option(Merge.inV, __.select("v")).toList()"#;
    both(two, script, |g, value| {
        assert_eq!(len(value), 2);
        assert_eq!(
            int(
                g,
                r#"g.V("1").out("self").has("name", "marko").count().next()"#
            ),
            1
        );
        assert_eq!(
            int(
                g,
                r#"g.V("2").out("self").has("name", "vadas").count().next()"#
            ),
            1
        );
    });
}

#[cfg(feature = "script")]
#[test]
fn dsl_merge_v_then_merge_e() {
    let script = r#"g.mergeV(#{"name": "marko", "T.label": "person"}).as("outV").mergeV(#{"name": "vadas", "T.label": "person"}).as("inV").mergeE(#{"Direction.OUT": Merge.outV, "Direction.IN": Merge.inV, "T.label": "knows"}).option(Merge.outV, __.select("outV")).option(Merge.inV, __.select("inV")).toList()"#;
    for graph in [empty as fn() -> Arc<Graph>, two] {
        both(graph, script, |g, value| {
            assert_eq!(len(value), 1);
            assert_eq!(int(g, "g.V().count().next()"), 2);
            assert_eq!(int(g, "g.E().count().next()"), 1);
            assert_eq!(
                int(
                    g,
                    r#"g.V().has("name", "marko").out("knows").has("name", "vadas").count().next()"#
                ),
                1
            );
        });
    }
}

#[cfg(feature = "script")]
#[test]
fn dsl_on_create_sets_the_edge_id() {
    both(
        two,
        r#"g.mergeE(#{"Direction.OUT": "1", "Direction.IN": "2", "T.label": "knows"}).option(Merge.onCreate, #{"T.id": "201"}).toList()"#,
        |g, value| {
            assert_eq!(len(value), 1);
            assert_eq!(int(g, r#"g.E("201").count().next()"#), 1);
        },
    );
    // The merge map names only the id; onCreate supplies the rest.
    both(
        two,
        r#"g.mergeE(#{"T.id": "201"}).option(Merge.onCreate, #{"Direction.OUT": "1", "Direction.IN": "2", "T.label": "knows"}).toList()"#,
        |g, _| {
            assert_eq!(int(g, r#"g.E("201").hasLabel("knows").count().next()"#), 1);
        },
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_mid_traversal_matches_every_edge() {
    // The incoming vertex is no implicit endpoint: the one marko traverser matches both knows
    // edges, and onMatch updates both.
    both(
        two_knows,
        r#"g.V().has("person", "name", "marko").mergeE(#{"T.label": "knows"}).option(Merge.onCreate, #{"created": "Y"}).option(Merge.onMatch, #{"created": "N"}).toList()"#,
        |g, value| {
            assert_eq!(len(value), 2);
            assert_eq!(int(g, r#"g.E().has("created", "N").count().next()"#), 2);
            assert_eq!(int(g, "g.E().count().next()"), 2);
        },
    );
    // Two incoming traversers, each matching both edges.
    both(
        two_knows,
        r#"g.V().mergeE(#{"T.label": "knows"}).toList()"#,
        |_, value| assert_eq!(len(value), 4),
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_on_match_traversal_receives_the_matched_edge() {
    both(
        two_knows,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2", "created": "Y"}).option(Merge.onMatch, __.project("seen").by(__.inV().values("name"))).toList()"#,
        |g, value| {
            assert_eq!(len(value), 1);
            assert_eq!(int(g, r#"g.E().has("seen", "vadas").count().next()"#), 1);
        },
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_errors() {
    for script in [r#"g.mergeE(#{}).toList()"#, r#"g.mergeE(()).toList()"#] {
        let err = both_err(two, script);
        assert!(err.contains("Out Vertex not specified"), "{err}");
    }
    let err = both_err(two, r#"g.mergeE(#{"Direction.OUT": "1"}).toList()"#);
    assert!(err.contains("In Vertex not specified"), "{err}");
    let err = both_err(
        two,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "100", "Direction.IN": "101"}).toList()"#,
    );
    assert!(
        err.contains("Vertex id could not be resolved from mergeE: 100"),
        "{err}"
    );
    let err = both_err(
        two,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2"}).option(Merge.onCreate, #{"Direction.OUT": "2"}).toList()"#,
    );
    assert!(
        err.contains("option(onCreate) cannot override values from merge() argument"),
        "{err}"
    );
    // A placeholder without its option, and option maps that do not identify one vertex.
    let err = both_err(
        two,
        r#"g.mergeE(#{"Direction.OUT": Merge.outV, "Direction.IN": "2"}).toList()"#,
    );
    assert!(err.contains("no option(Merge.outV, ..)"), "{err}");
    let err = both_err(
        two,
        r#"g.mergeE(#{"Direction.OUT": Merge.outV, "Direction.IN": "2"}).option(Merge.outV, #{"T.label": "person"}).toList()"#,
    );
    assert!(err.contains("matched 2 vertices"), "{err}");
    let err = both_err(
        two,
        r#"g.mergeE(#{"Direction.OUT": Merge.outV, "Direction.IN": "2"}).option(Merge.outV, #{"name": "nobody"}).toList()"#,
    );
    assert!(
        err.contains("Vertex id could not be resolved from mergeE"),
        "{err}"
    );
    // Direction.BOTH, a swapped placeholder, cardinality, and outV on mergeV.
    let err = both_err(two, r#"g.mergeE(#{"Direction.BOTH": "1"}).toList()"#);
    assert!(err.contains("Direction.BOTH"), "{err}");
    let err = both_err(two, r#"g.mergeE(#{"Direction.OUT": Merge.inV}).toList()"#);
    assert!(
        err.contains("cannot take the placeholder Merge.inV"),
        "{err}"
    );
    let err = both_err(
        two,
        r#"g.mergeE(#{"Direction.OUT": "1", "Direction.IN": "2", "w": Cardinality.single(1)}).toList()"#,
    );
    assert!(
        err.contains("Property cardinality can only be set for a Vertex"),
        "{err}"
    );
    let err = both_err(
        two,
        r#"g.mergeE(#{"Direction.OUT": "1", "Direction.IN": "2"}).option(Merge.onMatch, #{"w": 1}, Cardinality.single).toList()"#,
    );
    assert!(
        err.contains("Property cardinality can only be set for a Vertex"),
        "{err}"
    );
    let err = eval_value(
        two(),
        r#"g.mergeV(#{}).option(Merge.outV, #{"T.id": "1"}).toList()"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("merge_v() takes only option(Merge.onCreate"),
        "{err}"
    );
    // onMatch may not change the edge's token keys.
    let err = both_err(
        two_knows,
        r#"g.mergeE(#{"T.label": "knows"}).option(Merge.onMatch, #{"Direction.OUT": "2"}).toList()"#,
    );
    assert!(err.contains("option(onMatch) cannot change"), "{err}");
    assert_eq!(int(&two(), "g.E().count().next()"), 0);
}

#[cfg(feature = "script")]
#[test]
fn dsl_override_is_eager_for_constants_and_lazy_for_traversals() {
    // Both maps constant: fails although an edge matches.
    let err = both_err(
        two_knows,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "1"}).option(Merge.onCreate, #{"Direction.OUT": "2"}).toList()"#,
    );
    assert!(
        err.contains("option(onCreate) cannot override values from merge() argument"),
        "{err}"
    );
    // A dynamic onCreate is never evaluated when an edge matches.
    both(
        two_knows,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "1"}).option(Merge.onCreate, __.constant(#{"Direction.OUT": "2"})).toList()"#,
        |_, value| assert_eq!(len(value), 2),
    );
    // ... but fails when it is.
    let err = both_err(
        two,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "1"}).option(Merge.onCreate, __.constant(#{"Direction.OUT": "2"})).toList()"#,
    );
    assert!(
        err.contains("option(onCreate) cannot override values from merge() argument"),
        "{err}"
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_invalid_constant_map_fails_before_execution() {
    // No traverser reaches the step, the problem is still reported.
    let err = both_err(
        two,
        r#"g.V().has("name", "nobody").mergeE(#{"~label": "x"}).toList()"#,
    );
    assert!(
        err.contains("Property key can not be a hidden key: ~label"),
        "{err}"
    );
    let err = both_err(
        two,
        r#"g.V().has("name", "nobody").mergeE(#{}).option(Merge.outV, #{"Direction.OUT": "1"}).toList()"#,
    );
    assert!(
        err.contains("a vertex lookup map does not accept Direction keys"),
        "{err}"
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_merge_e_without_argument_merges_injected_maps() {
    both(
        two,
        r#"g.inject(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2"}).mergeE().toList()"#,
        |g, value| {
            assert_eq!(len(value), 1);
            assert_eq!(int(g, r#"g.V("1").out("knows").count().next()"#), 1);
        },
    );
}

#[cfg(feature = "script")]
#[test]
fn dsl_profile_shows_the_lookup() {
    let g = two_knows();
    let cases = [
        // Short maps, so the profile does not truncate the step name.
        (
            r#"g.mergeE(#{"Direction.OUT": "1"}).profile().to_string()"#,
            r#"merge_e(#{"Direction.OUT": "1"}) [lookup: out-edges]"#,
        ),
        (
            r#"g.mergeE(#{"Direction.IN": "2"}).profile().to_string()"#,
            "[lookup: in-edges]",
        ),
        (
            r#"g.mergeE(#{"T.id": "0"}).profile().to_string()"#,
            r#"merge_e(#{"T.id": "0"}) [lookup: id]"#,
        ),
        (
            r#"g.mergeE(#{"T.label": "knows"}).profile().to_string()"#,
            "[lookup: label]",
        ),
        (
            r#"g.mergeE(#{"created": "Y"}).profile().to_string()"#,
            "[lookup: scan]",
        ),
        (
            r#"g.inject(#{"T.label": "knows"}).mergeE().profile().to_string()"#,
            "[lookup: dynamic]",
        ),
    ];
    for (script, expected) in cases {
        // The T.id lookup creates an edge without endpoints and fails: a failing `profile()` is
        // a script error now, whose diagnostic still names the step.
        let profile = eval_value(g.clone(), script)
            .map(|value| value.to_string())
            .unwrap_or_else(|err| err.to_string());
        assert!(profile.contains(expected), "{script}: {profile}");
    }
}

#[cfg(feature = "script")]
#[test]
fn dsl_select_in_options_records_the_labels() {
    let profile = eval_value(
        two(),
        r#"g.V("1").as("x").V("2").as("y").mergeE(#{"Direction.OUT": Merge.outV, "Direction.IN": Merge.inV}).option(Merge.outV, __.select("x")).option(Merge.inV, __.select("y")).profile().to_string()"#,
    )
    .unwrap()
    .to_string();
    assert!(profile.contains(r#"as("x") [path: labels(x"#), "{profile}");
    assert!(
        profile.contains(r#"as("y") [path: labels(x,y)]"#),
        "{profile}"
    );
    assert!(profile.contains(r#"\> select("x")"#), "{profile}");
}

#[cfg(feature = "script")]
#[test]
fn dsl_count_still_mutates() {
    both(
        two,
        r#"g.mergeE(#{"T.label": "knows", "Direction.OUT": "1", "Direction.IN": "2"}).count().next()"#,
        |g, value| {
            assert_eq!(value.as_int().unwrap(), 1);
            assert_eq!(int(g, "g.E().count().next()"), 1);
        },
    );
}
