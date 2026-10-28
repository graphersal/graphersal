//! Integration tests for `repeat()` with `times()`, `until()`, `emit()` and `loops()`: TinkerPop
//! placement semantics, nested and named loops, cycles and limits, breadth- and depth-first
//! order, paths, profiling and the Rhai bindings.
//!
//! Expected results are those of TinkerPop 3.7 on the modern graph, compared as multisets unless
//! a test pins the documented order.

#[cfg(feature = "script")]
use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::profile::Metrics;

/// Names of the elements or values of a result, in result order.
fn names(values: &[ElementProperty]) -> Vec<String> {
    values
        .iter()
        .map(|value| match value {
            ElementProperty::String(s) => s.to_string(),
            ElementProperty::Number(n) => n.to_string(),
            ElementProperty::Vertex(v) => v
                .properties()
                .get("name")
                .and_then(|n| n.as_string())
                .unwrap_or("?")
                .to_string(),
            ElementProperty::Array(items) => format!("{:?}", names(items)),
            other => format!("{other:?}"),
        })
        .collect()
}

/// The names of a result as a sorted multiset.
fn multiset(values: &[ElementProperty]) -> Vec<String> {
    let mut names = names(values);
    names.sort();
    names
}

fn sorted(expected: &[&str]) -> Vec<String> {
    let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    expected.sort();
    expected
}

/// Runs a Rhai query that ends on a traversal and returns its materialized results.
#[cfg(feature = "script")]
fn rhai(graph: &Arc<Graph>, script: &str) -> TraverserResult<Vec<ElementProperty>> {
    let engine = crate::script_host::engine();
    let mut scope = crate::script_host::graph_scope(graph.clone());
    let gts = engine
        .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
        .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
    let lock = graph.read();
    gts.ast_convert_to_gts(&*lock).to_list()
}

/// Runs the Rust traversal, then asserts that every Rhai spelling returns exactly its result.
fn dual<F>(graph: Graph, rust: F, scripts: &[&str]) -> Vec<ElementProperty>
where
    F: for<'b> FnOnce(
        GraphTraversalSource<'b, &'b TraversalGraph>,
    ) -> TraverserResult<Vec<ElementProperty>>,
{
    let result = {
        let lock = graph.read();
        rust(lock.traversal()).expect("Rust traversal failed")
    };
    #[cfg(feature = "script")]
    {
        let graph = Arc::new(graph);
        for script in scripts {
            let script_result =
                rhai(&graph, script).unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
            assert_eq!(result, script_result, "Rhai `{script}` differs from Rust");
        }
    }
    #[cfg(not(feature = "script"))]
    let _ = (graph, scripts);
    result
}

/// Like [`dual`], comparing the Rhai results as multisets: for steps whose output order is not
/// defined, such as `dedup()`.
fn dual_unordered<F>(graph: Graph, rust: F, scripts: &[&str]) -> Vec<ElementProperty>
where
    F: for<'b> FnOnce(
        GraphTraversalSource<'b, &'b TraversalGraph>,
    ) -> TraverserResult<Vec<ElementProperty>>,
{
    let result = {
        let lock = graph.read();
        rust(lock.traversal()).expect("Rust traversal failed")
    };
    #[cfg(feature = "script")]
    {
        let graph = Arc::new(graph);
        for script in scripts {
            let script_result =
                rhai(&graph, script).unwrap_or_else(|e| panic!("`{script}` failed: {e}"));
            assert_eq!(
                multiset(&result),
                multiset(&script_result),
                "Rhai `{script}` differs from Rust"
            );
        }
    }
    #[cfg(not(feature = "script"))]
    let _ = (graph, scripts);
    result
}

fn modern() -> Graph {
    GraphSource::tinkerpop_modern()
}

/// Builds a graph of vertices named after their ids, with `child` edges.
///
/// `out()` yields neighbours in reverse edge insertion order, so the edges are inserted in
/// reverse to make `out()` follow the listed order.
fn graph_of(vertices: &[&str], edges: &[(&str, &str)]) -> Graph {
    let mut graph = TraversalGraph::new();
    let mut ids = std::collections::HashMap::new();
    for id in vertices {
        let mut props = graphersal::prelude::PropertyMap::default();
        props.insert("name", ElementProperty::String((*id).into()));
        ids.insert(
            *id,
            graph
                .add_vertex(Some((*id).into()), vec!["node".into()], props)
                .unwrap(),
        );
    }
    for (from, to) in edges.iter().rev() {
        graph
            .add_edge(
                None,
                Some("child".into()),
                ids[from],
                ids[to],
                Default::default(),
            )
            .unwrap();
    }
    Graph::new(graph)
}

/// a -> b -> c -> a
fn cycle() -> Graph {
    graph_of(&["a", "b", "c"], &[("a", "b"), ("b", "c"), ("c", "a")])
}

/// r -> (a -> (a1, a2), b -> b1)
fn tree() -> Graph {
    graph_of(
        &["r", "a", "b", "a1", "a2", "b1"],
        &[
            ("r", "a"),
            ("r", "b"),
            ("a", "a1"),
            ("a", "a2"),
            ("b", "b1"),
        ],
    )
}

fn dfs<'b>(
    g: GraphTraversalSource<'b, &'b TraversalGraph>,
) -> GraphTraversalSource<'b, &'b TraversalGraph> {
    g.with("repeat.order", "dfs")
}

// T1: the motivating examples.

#[test]
fn repeat_out_times_two() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .times(2)
                .values("name")
                .to_list()
        },
        &[
            r#"g.V("1").repeat(__.out()).times(2).values("name")"#,
            r#"g.v("1").repeat(__.out()).times(2).values("name")"#,
        ],
    );
    assert_eq!(multiset(&res), sorted(&["ripple", "lop"]));
}

#[test]
fn repeat_out_times_three_is_empty() {
    let res = dual(
        modern(),
        |g| g.v("1").repeat(__::out(None)).times(3).to_list(),
        &[r#"g.V("1").repeat(__.out()).times(3)"#],
    );
    assert!(res.is_empty(), "{res:?}");
}

#[test]
fn repeat_until_path() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .until(__::has("name", "ripple"))
                .path()
                .by("name")
                .to_list()
        },
        &[
            r#"g.V("1").repeat(__.out()).until(__.has("name", "ripple")).path().by("name")"#,
            r#"g.v("1").repeat(__.out()).until(__.has("name", "ripple")).path().by("name")"#,
        ],
    );
    assert_eq!(names(&res), vec![r#"["marko", "josh", "ripple"]"#]);
}

#[test]
fn emit_before_repeat_out_knows() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .emit()
                .repeat(__::out("knows"))
                .times(2)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").emit().repeat(__.out("knows")).times(2).values("name")"#],
    );
    // Breadth first: marko before the loop, then josh and vadas in out("knows") order.
    assert_eq!(names(&res), vec!["marko", "josh", "vadas"]);
}

// T2: placement matrix.

#[test]
fn times_before_repeat_is_checked_first() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .times(2)
                .repeat(__::out(None))
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").times(2).repeat(__.out()).values("name")"#],
    );
    assert_eq!(multiset(&res), sorted(&["ripple", "lop"]));
}

#[test]
fn prefix_emit_with_postfix_times() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .emit()
                .repeat(__::out(None))
                .times(2)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").emit().repeat(__.out()).times(2).values("name")"#],
    );
    assert_eq!(
        multiset(&res),
        sorted(&["marko", "lop", "vadas", "josh", "ripple", "lop"])
    );
}

#[test]
fn postfix_emit_with_postfix_times() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .emit()
                .times(2)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).emit().times(2).values("name")"#],
    );
    assert_eq!(
        multiset(&res),
        sorted(&["lop", "vadas", "josh", "ripple", "lop"])
    );
}

#[test]
fn postfix_emit_with_postfix_until() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .emit()
                .until(__::has("name", "ripple"))
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).emit().until(__.has("name", "ripple")).values("name")"#],
    );
    // ripple stops the loop and is output once, not also through emit().
    assert_eq!(
        multiset(&res),
        sorted(&["lop", "vadas", "josh", "ripple", "lop"])
    );
}

#[test]
fn postfix_emit_and_until_both_true_output_once() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .emit()
                .until(__::has_label("software"))
                .values("name")
                .to_list()
        },
        &[
            r#"g.V("1").repeat(__.out()).emit().until(__.hasLabel("software")).values("name")"#,
            r#"g.V("1").repeat(__.out()).emit().until(__.has_label("software")).values("name")"#,
        ],
    );
    assert_eq!(
        multiset(&res),
        sorted(&["lop", "vadas", "josh", "ripple", "lop"])
    );
}

#[test]
fn prefix_emit_and_until_both_true_output_once() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .emit()
                .until(__::has_label("person"))
                .repeat(__::out(None))
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").emit().until(__.hasLabel("person")).repeat(__.out()).values("name")"#],
    );
    assert_eq!(names(&res), vec!["marko"]);
}

#[test]
fn prefix_until_before_repeat() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .until(__::has("name", "ripple"))
                .repeat(__::out(None))
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").until(__.has("name", "ripple")).repeat(__.out()).values("name")"#],
    );
    assert_eq!(names(&res), vec!["ripple"]);
}

#[test]
fn emit_without_stop_condition() {
    let prefix = dual(
        modern(),
        |g| {
            g.v("1")
                .emit()
                .repeat(__::out(None))
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").emit().repeat(__.out()).values("name")"#],
    );
    assert_eq!(
        multiset(&prefix),
        sorted(&["marko", "lop", "vadas", "josh", "ripple", "lop"])
    );
    let postfix = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .emit()
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).emit().values("name")"#],
    );
    assert_eq!(
        multiset(&postfix),
        sorted(&["lop", "vadas", "josh", "ripple", "lop"])
    );
}

#[test]
fn a_modulator_after_a_complete_repeat_starts_a_new_one() {
    // The emit() belongs to the first repeat(); the second repeat() is fresh and takes times(1).
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .times(2)
                .emit()
                .repeat(__::in_(None))
                .times(1)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).times(2).emit().repeat(__.in()).times(1).values("name")"#],
    );
    // The first loop outputs lop, vadas, josh, ripple, lop; one in() step from each.
    let mut expected = vec!["marko"; 4];
    expected.extend(["josh"; 3]);
    expected.extend(["peter"; 2]);
    assert_eq!(multiset(&res), sorted(&expected));
}

#[test]
fn times_zero_after_repeat_runs_the_body_once() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .times(0)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).times(0).values("name")"#],
    );
    assert_eq!(multiset(&res), sorted(&["lop", "vadas", "josh"]));
}

#[test]
fn times_zero_before_repeat_passes_the_input_through() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .times(0)
                .repeat(__::out(None))
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").times(0).repeat(__.out()).values("name")"#],
    );
    assert_eq!(names(&res), vec!["marko"]);
}

#[test]
fn a_condition_set_twice_is_replaced() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .times(3)
                .times(1)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).times(3).times(1).values("name")"#],
    );
    assert_eq!(multiset(&res), sorted(&["lop", "vadas", "josh"]));
}

#[test]
fn a_body_that_yields_nothing_drops_the_traverser() {
    // Without emit() the traversers the body filters out are gone.
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .until(__::has("name", "nobody"))
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).until(__.has("name", "nobody"))"#],
    );
    assert!(res.is_empty(), "{res:?}");
}

// T3: loops().

#[test]
fn until_loops_is_times() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .until(__::loops().is(2))
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).until(__.loops().is(2)).values("name")"#],
    );
    assert_eq!(multiset(&res), sorted(&["ripple", "lop"]));
}

#[test]
fn loops_inside_the_body_is_the_current_iteration() {
    // Iteration 0 passes where(loops() is 0); iteration 1 does not.
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .emit()
                .repeat(__::out(None).where_t(__::loops().is(0)))
                .times(2)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").emit().repeat(__.out().where(__.loops().is(0))).times(2).values("name")"#],
    );
    assert_eq!(multiset(&res), sorted(&["marko", "lop", "vadas", "josh"]));

    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .emit()
                .repeat(__::out(None).where_t(__::loops().is(1)))
                .times(2)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").emit().repeat(__.out().where(__.loops().is(1))).times(2).values("name")"#],
    );
    assert_eq!(names(&res), vec!["marko"]);
}

#[test]
fn loops_outside_a_repeat_is_zero() {
    let res = dual(
        modern(),
        |g| g.v("1").loops().to_list(),
        &[r#"g.V("1").loops()"#],
    );
    assert_eq!(res, vec![ElementProperty::Number(0.into())]);
    // The counter is reset once a traverser leaves the loop.
    let res = dual(
        modern(),
        |g| g.v("1").repeat(__::out(None)).times(1).loops().to_list(),
        &[r#"g.V("1").repeat(__.out()).times(1).loops()"#],
    );
    assert_eq!(res, vec![ElementProperty::Number(0.into()); 3]);
}

#[test]
fn emit_on_a_loops_predicate() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .emit_with(__::loops().is(P::Gt(0)))
                .repeat(__::out(None))
                .values("name")
                .to_list()
        },
        &[
            r#"g.V("1").emit(__.loops().is(P.gt(0))).repeat(__.out()).values("name")"#,
            r#"g.V("1").emit(__.loops().is(gt(0))).repeat(__.out()).values("name")"#,
        ],
    );
    // Not marko: before the first iteration loops() is 0.
    assert_eq!(
        multiset(&res),
        sorted(&["lop", "vadas", "josh", "ripple", "lop"])
    );
}

// T4: nested loops.

fn nested_out_in_expected() -> Vec<String> {
    let mut expected = vec!["marko"; 11];
    expected.extend(["josh"; 6]);
    expected.extend(["peter"; 5]);
    sorted(&expected)
}

#[test]
fn nested_anonymous_loops() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None).repeat(__::in_(None)).times(1))
                .times(2)
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out().repeat(__.in()).times(1)).times(2).values("name")"#],
    );
    assert_eq!(multiset(&res), nested_out_in_expected());
}

#[test]
fn nested_named_loops_read_their_own_counters() {
    // loops("b") is the inner counter: the same as the anonymous nesting.
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat_named(
                    "a",
                    __::out(None)
                        .repeat_named("b", __::in_(None))
                        .until(__::loops_named("b").is(1)),
                )
                .times(2)
                .values("name")
                .to_list()
        },
        &[
            r#"g.V("1").repeat("a", __.out().repeat("b", __.in()).until(__.loops("b").is(1))).times(2).values("name")"#,
        ],
    );
    assert_eq!(multiset(&res), nested_out_in_expected());

    // Inside the inner loop, loops("a") is the outer counter: 0 during the first outer iteration.
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat_named(
                    "a",
                    __::out(None)
                        .repeat_named("b", __::in_(None))
                        .until(__::loops_named("a").is(0)),
                )
                .times(1)
                .values("name")
                .to_list()
        },
        &[
            r#"g.V("1").repeat("a", __.out().repeat("b", __.in()).until(__.loops("a").is(0))).times(1).values("name")"#,
        ],
    );
    assert_eq!(
        multiset(&res),
        sorted(&["marko", "josh", "peter", "marko", "marko"])
    );

    // The spec example: loops("a") is 0 during the first outer iteration, so the inner loop never
    // meets its until() and walks in() until the body yields nothing: every traverser is dropped.
    let res = dual(
        modern(),
        |g| {
            g.v(None)
                .repeat_named(
                    "a",
                    __::out(None)
                        .repeat_named("b", __::in_(None))
                        .until(__::loops_named("a").is(1)),
                )
                .times(2)
                .to_list()
        },
        &[
            r#"g.V().repeat("a", __.out().repeat("b", __.in()).until(__.loops("a").is(1))).times(2)"#,
        ],
    );
    assert!(res.is_empty(), "{res:?}");
}

#[test]
fn an_unknown_loop_name_lists_the_active_loops() {
    let graph = modern();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("1")
        .repeat_named(
            "a",
            __::out(None)
                .repeat_named("b", __::in_(None).where_t(__::loops_named("zzz").is(0)))
                .times(1),
        )
        .times(1)
        .to_list()
        .expect_err("zzz names no loop");
    match err.root_cause() {
        TraverserError::UnknownLoopName { name, active, .. } => {
            assert_eq!(name, "zzz");
            assert_eq!(active, &vec!["b".to_string(), "a".to_string()]);
        }
        other => panic!("expected UnknownLoopName, got {other:?}"),
    }
    let report = format_diagnostic(&err);
    assert!(
        report.contains(r#"loops("zzz") names no enclosing repeat() (active loops: b, a)"#),
        "{report}"
    );
}

#[test]
fn emit_on_both_levels() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .emit()
                .repeat(__::out(None).emit().repeat(__::in_(None)).times(1))
                .times(1)
                .values("name")
                .to_list()
        },
        &[
            r#"g.V("1").emit().repeat(__.out().emit().repeat(__.in()).times(1)).times(1).values("name")"#,
        ],
    );
    assert_eq!(
        multiset(&res),
        sorted(&[
            "marko", "lop", "vadas", "josh", "marko", "josh", "peter", "marko", "marko"
        ])
    );
}

#[test]
fn nested_loops_depth_first_give_the_same_multisets() {
    let res = dual(
        modern(),
        |g| {
            dfs(g)
                .v("1")
                .repeat(__::out(None).repeat(__::in_(None)).times(1))
                .times(2)
                .values("name")
                .to_list()
        },
        &[
            r#"g.with("repeat.order", "dfs").V("1").repeat(__.out().repeat(__.in()).times(1)).times(2).values("name")"#,
        ],
    );
    assert_eq!(multiset(&res), nested_out_in_expected());

    let res = dual(
        modern(),
        |g| {
            dfs(g)
                .v("1")
                .emit()
                .repeat(__::out(None).emit().repeat(__::in_(None)).times(1))
                .times(1)
                .values("name")
                .to_list()
        },
        &[
            r#"g.with("repeat.order", "dfs").V("1").emit().repeat(__.out().emit().repeat(__.in()).times(1)).times(1).values("name")"#,
        ],
    );
    assert_eq!(
        multiset(&res),
        sorted(&[
            "marko", "lop", "vadas", "josh", "marko", "josh", "peter", "marko", "marko"
        ])
    );
}

// T5: cycles, limits and timeouts.

#[test]
fn an_unbounded_loop_on_a_cycle_hits_the_loop_limit() {
    let res = cycle();
    let lock = res.read();
    let err = lock
        .traversal()
        .with("repeat.max_loops", 50)
        .v("a")
        .repeat(__::out(None))
        .to_list()
        .expect_err("a -> b -> c -> a never ends");
    match err.root_cause() {
        TraverserError::RepeatLimitExceeded {
            max_loops,
            loop_name,
            body,
            ..
        } => {
            assert_eq!(*max_loops, 50);
            assert_eq!(loop_name, &None);
            assert_eq!(body, "__.out()");
        }
        other => panic!("expected RepeatLimitExceeded, got {other:?}"),
    }
    // Reported against the repeat() step.
    assert!(
        format_diagnostic(&err).starts_with("Error: Step #1 'repeat(__.out())' execution failed"),
        "{}",
        format_diagnostic(&err)
    );
}

#[test]
fn the_default_loop_limit_applies_without_an_option() {
    let graph = cycle();
    let lock = graph.read();
    let err = lock
        .traversal()
        .v("a")
        .repeat_named("walk", __::out(None))
        .until(__::has("name", "nowhere"))
        .to_list()
        .expect_err("the cycle never meets the condition");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::RepeatLimitExceeded { max_loops: 10_000, loop_name: Some(name), .. }
                if name == "walk"
        ),
        "{err:?}"
    );
}

#[test]
fn simple_path_ends_a_loop_on_a_cycle() {
    let res = dual(
        cycle(),
        |g| {
            g.v("a")
                .repeat(__::out(None).simple_path())
                .emit()
                .path()
                .by("name")
                .to_list()
        },
        &[
            r#"g.V("a").repeat(__.out().simplePath()).emit().path().by("name")"#,
            r#"g.v("a").repeat(__.out().simple_path()).emit().path().by("name")"#,
        ],
    );
    assert_eq!(names(&res), vec![r#"["a", "b"]"#, r#"["a", "b", "c"]"#]);
}

#[test]
fn times_is_never_limited_by_max_loops() {
    let res = dual(
        cycle(),
        |g| {
            g.with("repeat.max_loops", 5)
                .v("a")
                .repeat(__::out(None))
                .times(100)
                .values("name")
                .to_list()
        },
        &[r#"g.with("repeat.max_loops", 5).V("a").repeat(__.out()).times(100).values("name")"#],
    );
    // 100 hops around a three-cycle end one step after the start.
    assert_eq!(names(&res), vec!["b"]);
}

#[test]
fn a_long_times_loop_is_stopped_by_the_timeout() {
    let graph = cycle();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("evaluationTimeout", 1)
        .v("a")
        .repeat(__::out(None))
        .times(100_000_000)
        .to_list()
        .expect_err("100M iterations exceed 1 ms");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::Timeout { limit_ms: 1, .. }
        ),
        "{err:?}"
    );
}

#[test]
fn a_loop_counter_limit_stops_depth_first_order_too() {
    let graph = cycle();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("repeat.order", "dfs")
        .with("repeat.max_loops", 7)
        .v("a")
        .emit()
        .repeat(__::out(None))
        .to_list()
        .expect_err("the cycle never ends");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::RepeatLimitExceeded { max_loops: 7, .. }
        ),
        "{err:?}"
    );
}

#[test]
fn invalid_repeat_options_are_rejected() {
    let graph = modern();
    let lock = graph.read();
    for (key, value) in [
        ("repeat.order", ElementProperty::from("sideways")),
        ("repeat.max_loops", ElementProperty::from(0)),
        ("repeat.max_loops", ElementProperty::from(-5)),
    ] {
        let err = lock
            .traversal()
            .with(key, value)
            .v("1")
            .repeat(__::out(None))
            .times(1)
            .to_list()
            .expect_err("invalid option");
        assert!(
            matches!(&*err, TraverserError::InvalidOption { key: k, .. } if k == key),
            "{err:?}"
        );
    }
}

// T6: breadth first versus depth first.

#[test]
fn breadth_and_depth_first_return_the_same_multisets() {
    type Query = for<'b> fn(
        GraphTraversalSource<'b, &'b TraversalGraph>,
    ) -> GraphTraversalSource<'b, &'b TraversalGraph>;
    let queries: Vec<Query> = vec![
        |g| g.v("1").repeat(__::out(None)).times(2).values("name"),
        |g| {
            g.v("1")
                .emit()
                .repeat(__::out(None))
                .times(2)
                .values("name")
        },
        |g| g.v("1").repeat(__::out(None)).emit().values("name"),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .until(__::has("name", "ripple"))
                .values("name")
        },
        |g| {
            g.v(None)
                .repeat(__::both(None).simple_path())
                .times(2)
                .values("name")
        },
        |g| {
            g.v("1")
                .repeat(__::out(None).repeat(__::in_(None)).times(1))
                .times(2)
                .values("name")
        },
    ];
    let graph = modern();
    let lock = graph.read();
    for query in queries {
        let bfs = query(lock.traversal()).to_list().unwrap();
        let dfs = query(lock.traversal().with("repeat.order", "dfs"))
            .to_list()
            .unwrap();
        assert!(!bfs.is_empty());
        assert_eq!(multiset(&bfs), multiset(&dfs));
    }
}

#[test]
fn breadth_first_order_is_level_by_level() {
    let res = dual(
        tree(),
        |g| {
            g.v("r")
                .emit()
                .repeat(__::out(None))
                .values("name")
                .to_list()
        },
        &[
            r#"g.V("r").emit().repeat(__.out()).values("name")"#,
            r#"g.with("repeat.order", "bfs").V("r").emit().repeat(__.out()).values("name")"#,
        ],
    );
    assert_eq!(names(&res), vec!["r", "a", "b", "a1", "a2", "b1"]);
}

#[test]
fn depth_first_order_is_pre_order() {
    let res = dual(
        tree(),
        |g| {
            dfs(g)
                .v("r")
                .emit()
                .repeat(__::out(None))
                .values("name")
                .to_list()
        },
        &[
            r#"g.with("repeat.order", "dfs").V("r").emit().repeat(__.out()).values("name")"#,
            r#"g.with("repeat.order", "DFS").v("r").emit().repeat(__.out()).values("name")"#,
        ],
    );
    assert_eq!(names(&res), vec!["r", "a", "a1", "a2", "b", "b1"]);

    // Postfix checks run on the children of a traverser as soon as it is expanded.
    let res = dual(
        tree(),
        |g| {
            dfs(g)
                .v("r")
                .repeat(__::out(None))
                .emit()
                .values("name")
                .to_list()
        },
        &[r#"g.with("repeat.order", "dfs").V("r").repeat(__.out()).emit().values("name")"#],
    );
    assert_eq!(names(&res), vec!["a", "b", "a1", "a2", "b1"]);
}

// T7: paths and labels.

#[test]
fn a_label_in_the_body_selects_the_last_iteration() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None).as_("x"))
                .times(2)
                .select("x")
                .values("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out().as("x")).times(2).select("x").values("name")"#],
    );
    assert_eq!(multiset(&res), sorted(&["ripple", "lop"]));
}

#[test]
fn path_after_a_loop_shows_the_whole_route() {
    let res = dual(
        modern(),
        |g| {
            g.v("1")
                .repeat(__::out(None))
                .times(2)
                .path()
                .by("name")
                .to_list()
        },
        &[r#"g.V("1").repeat(__.out()).times(2).path().by("name")"#],
    );
    assert_eq!(
        multiset(&res),
        sorted(&[
            r#"["marko", "josh", "ripple"]"#,
            r#"["marko", "josh", "lop"]"#
        ])
    );
    // The analysis kill switch records the same paths.
    let graph = modern();
    let lock = graph.read();
    let full = lock
        .traversal()
        .with("path.analysis", false)
        .v("1")
        .repeat(__::out(None))
        .times(2)
        .path()
        .by("name")
        .to_list()
        .unwrap();
    assert_eq!(full, res);
}

/// The step names as the profile table shows them: the name, then the path recording mode.
fn shown(n: &Metrics) -> String {
    match n.path_recording() {
        Some(recording) => format!("{} [path: {recording}]", n.name()),
        None => n.name().to_string(),
    }
}

fn step_names(nodes: &[Metrics]) -> Vec<String> {
    nodes.iter().map(shown).collect()
}

#[test]
fn a_path_reader_in_the_body_makes_the_loop_record_full_paths() {
    let graph = modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v("1")
        .repeat(__::out(None).simple_path())
        .times(3)
        .values("name")
        .profile()
        .unwrap();
    let nodes = &stats.metrics();
    assert_eq!(
        step_names(nodes),
        vec![
            r#"v("1") [path: full]"#,
            "repeat(__.out().simple_path()).times(3)",
            r#"values("name")"#,
        ]
    );
    assert_eq!(
        step_names(nodes[1].nested()),
        vec!["out() [path: full]", "simple_path() [path: full]"]
    );
}

#[test]
fn a_path_reader_in_until_reaches_the_body_and_the_input() {
    let graph = modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v("1")
        .repeat(__::out(None))
        .until(__::simple_path().count().is(0))
        .count()
        .profile()
        .unwrap();
    let nodes = &stats.metrics();
    assert!(
        shown(&nodes[0]).ends_with("[path: full]"),
        "{:?}",
        step_names(nodes)
    );
    assert_eq!(step_names(nodes[1].nested())[0], "out() [path: full]");
}

#[test]
fn a_loop_without_path_readers_records_nothing() {
    let graph = modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v("1")
        .repeat(__::out(None))
        .times(2)
        .values("name")
        .profile()
        .unwrap();
    let all: Vec<String> = stats
        .metrics()
        .iter()
        .flat_map(|n| std::iter::once(shown(n)).chain(step_names(n.nested())))
        .collect();
    assert!(all.iter().all(|name| !name.contains("[path:")), "{all:?}");
}

// T8: dedup() inside the body keeps one seen-set for the whole loop, like TinkerPop.

#[test]
fn dedup_in_the_body_keeps_its_seen_set_across_iterations() {
    // s -> a, s -> b, a -> b, b -> c. The second `b` (from a) and the second `c` are dropped.
    let graph = graph_of(
        &["s", "a", "b", "c"],
        &[("s", "a"), ("s", "b"), ("a", "b"), ("b", "c")],
    );
    let res = dual_unordered(
        graph,
        |g| {
            g.v("s")
                .repeat(__::out(None).dedup())
                .emit()
                .values("name")
                .to_list()
        },
        &[r#"g.V("s").repeat(__.out().dedup()).emit().values("name")"#],
    );
    assert_eq!(multiset(&res), sorted(&["a", "b", "c"]));

    // The diamond: s -> a -> t and s -> b -> t meet once.
    let diamond = graph_of(
        &["s", "a", "b", "t"],
        &[("s", "a"), ("s", "b"), ("a", "t"), ("b", "t")],
    );
    let res = dual_unordered(
        diamond,
        |g| {
            g.v("s")
                .repeat(__::out(None).dedup())
                .times(2)
                .values("name")
                .to_list()
        },
        &[r#"g.V("s").repeat(__.out().dedup()).times(2).values("name")"#],
    );
    assert_eq!(names(&res), vec!["t"]);
}

// T9: mutation inside the body, through Rhai.

#[cfg(feature = "script")]
#[test]
fn a_mutating_body_updates_the_graph() {
    let graph = Arc::new(modern());
    let _ = crate::script_host::eval_value(
        graph.clone(),
        r#"g.V("1").repeat(__.out().property("seen", true)).times(2).to_list()"#,
    )
    .expect("the mutating loop runs");
    let lock = graph.read();
    let seen = lock
        .traversal()
        .v(None)
        .has("seen", true)
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(multiset(&seen), sorted(&["lop", "vadas", "josh", "ripple"]));
}

// T10: profile.

#[test]
fn profile_names_the_written_shape() {
    let graph = modern();
    let lock = graph.read();
    let shapes = [
        (
            lock.traversal()
                .v("1")
                .emit()
                .repeat(__::out(None))
                .times(2)
                .profile(),
            "emit().repeat(__.out()).times(2)",
        ),
        (
            lock.traversal()
                .v("1")
                .repeat(__::out(None))
                .until(__::has("name", "ripple"))
                .profile(),
            r#"repeat(__.out()).until(__.has("name", "ripple"))"#,
        ),
        (
            lock.traversal()
                .v("1")
                .repeat_named("a", __::out(None))
                .times(1)
                .profile(),
            r#"repeat("a", __.out()).times(1)"#,
        ),
        (
            lock.traversal()
                .v("1")
                .times(1)
                .repeat(__::out(None))
                .emit()
                .profile(),
            "times(1).repeat(__.out()).emit()",
        ),
    ];
    for (stats, expected) in shapes {
        let stats = stats.unwrap();
        assert_eq!(stats.metrics()[1].name(), expected);
        assert!(stats.to_string().contains(expected));
    }
}

#[test]
fn profile_accumulates_children_and_summarises_the_loop() {
    let graph = modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v("1")
        .repeat(__::out(None))
        .times(2)
        .profile()
        .unwrap();
    let repeat = &stats.metrics()[1];
    assert_eq!(
        (repeat.count_in(), repeat.counts().traverser_count()),
        (1, 2)
    );
    let out = &repeat.nested()[0];
    assert_eq!(out.name(), "out()");
    // Iteration 0: marko -> 3; iteration 1: 3 -> 2.
    assert_eq!(
        (out.calls(), out.count_in(), out.counts().traverser_count()),
        (2, 4, 5)
    );
    let loops = repeat.loops().unwrap();
    assert_eq!((loops.total_loops(), loops.max_depth()), (2, 2));
    assert!((loops.avg_loops_per_traverser() - 2.0).abs() < 1e-9);
    assert!(
        stats
            .to_string()
            .contains("[loops: 2, max_depth: 2, avg_loops: 2.0]")
    );
}

#[test]
fn profile_places_condition_children_after_the_body() {
    let graph = modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v("1")
        .repeat(__::out(None))
        .until(__::has("name", "ripple"))
        .emit_with(__::has_label("person"))
        .profile()
        .unwrap();
    let repeat = &stats.metrics()[1];
    assert_eq!(
        step_names(repeat.nested()),
        vec!["out()", r#"has("name", "ripple")"#, "has_label(\"person\")"]
    );
    // Emitted: vadas, josh (loops 1); stopped: ripple (loops 2).
    let summary = repeat.loops().unwrap();
    assert_eq!((summary.total_loops(), summary.max_depth()), (3, 3));
    assert!((summary.avg_loops_per_traverser() - 4.0 / 3.0).abs() < 1e-9);
}

#[test]
fn depth_first_profile_counts_expanded_traversers() {
    let graph = modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .with("repeat.order", "dfs")
        .v("1")
        .repeat(__::out(None))
        .times(2)
        .profile()
        .unwrap();
    let summary = stats.metrics()[1].loops().unwrap();
    // marko, then lop, vadas, josh.
    assert_eq!((summary.total_loops(), summary.max_depth()), (4, 2));
}

// T11: errors (help texts are asserted in error_diagnostics_tests.rs).

#[test]
fn a_modulator_without_repeat_fails_at_execution() {
    let graph = modern();
    let lock = graph.read();
    for (query, modulator) in [
        (lock.traversal().v(None).times(2), "times(2)"),
        (lock.traversal().v(None).emit(), "emit()"),
        (
            lock.traversal().v(None).until(__::has_label("person")),
            r#"until(__.has_label("person"))"#,
        ),
        (lock.traversal().v(None).emit().times(1), "emit().times(1)"),
    ] {
        let err = query.to_list().expect_err("no repeat() to modulate");
        match err.root_cause() {
            TraverserError::RepeatWithoutBody { modulator: m, .. } => assert_eq!(m, modulator),
            other => panic!("expected RepeatWithoutBody, got {other:?}"),
        }
    }
}

// T12: Rhai parsing.

#[cfg(feature = "script")]
#[test]
fn until_is_a_step_in_method_position_and_a_keyword_elsewhere() {
    let graph = Arc::new(modern());
    let res = rhai(
        &graph,
        r#"g.V("1").repeat(__.out()).until(__.has("name", "ripple")).values("name")"#,
    )
    .unwrap();
    assert_eq!(names(&res), vec!["ripple"]);
    let res = rhai(
        &graph,
        r#"g.V("1").repeat(__.out()).until(__.until(__.has("name", "ripple")).repeat(__.in())).values("name")"#,
    );
    assert!(res.is_ok(), "{res:?}");

    let value =
        crate::script_host::eval_value(graph.clone(), "let x = 0; do { x += 1; } until x > 3; x")
            .unwrap();
    assert_eq!(value.as_int().unwrap(), 4);
}

#[cfg(feature = "script")]
#[test]
fn times_rejects_negative_and_oversized_counts() {
    let graph = Arc::new(modern());
    for script in [
        r#"g.V("1").repeat(__.out()).times(-1)"#,
        r#"g.V("1").repeat(__.out()).times(5000000000)"#,
    ] {
        let err = crate::script_host::eval_value(graph.clone(), script).expect_err(script);
        assert!(err.to_string().contains("times() expects"), "{err}");
    }
}

#[cfg(feature = "script")]
#[test]
fn every_arity_is_registered() {
    let names = crate::script_host::registered_step_names();
    for step in ["repeat", "times", "until", "emit", "loops"] {
        assert!(
            names.contains(&step.to_string()),
            "{step} is not registered"
        );
    }
    let graph = Arc::new(modern());
    for script in [
        r#"__.repeat(__.out()).times(1)"#,
        r#"__.repeat("a", __.out()).times(1)"#,
        r#"__.times(1).repeat(__.out())"#,
        r#"__.until(__.loops().is(1)).repeat(__.out())"#,
        r#"__.emit().repeat(__.out()).times(1)"#,
        r#"__.emit(__.hasLabel("person")).repeat(__.out()).times(1)"#,
        r#"__.loops()"#,
        r#"__.loops("a")"#,
    ] {
        let _ = crate::script_host::eval_value(graph.clone(), script)
            .unwrap_or_else(|e| panic!("`{script}` does not parse: {e}"));
    }
}

// T13: optimizer.

#[test]
fn a_pushdown_applies_inside_the_body() {
    let graph = modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v("1")
        .repeat(__::out_e(None).has_label("knows").in_v())
        .times(1)
        .values("name")
        .execute_with(ExecOptions::profile(ProfileType::none()));
    let data = stats.results().to_vec();
    let stats = stats.into_profile().unwrap();
    let repeat = &stats.metrics()[1];
    assert_eq!(
        repeat.name(),
        r#"repeat(__.out_e("knows").in_v()).times(1)"#
    );
    assert_eq!(
        step_names(repeat.nested()),
        vec![r#"out_e("knows")"#, "in_v()"]
    );
    assert_eq!(multiset(&data), sorted(&["vadas", "josh"]));
}

#[test]
fn no_step_moves_across_a_repeat() {
    let graph = modern();
    let lock = graph.read();
    let stats = lock
        .traversal()
        .v(None)
        .repeat(__::out(None))
        .times(1)
        .has_label("person")
        .has_id("2")
        .count()
        .execute_with(ExecOptions::profile(ProfileType::none()));
    let data = stats.results().to_vec();
    let stats = stats.into_profile().unwrap();
    let top = step_names(stats.metrics());
    assert_eq!(top[0], "v()", "{top:?}");
    assert_eq!(top[1], "repeat(__.out()).times(1)", "{top:?}");
    assert_eq!(data, vec![ElementProperty::Number(1.into())]);
}
