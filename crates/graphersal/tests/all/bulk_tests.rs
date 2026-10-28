//! Traverser bulk. Step 1 plumbed the field and the options; step 2 added the merge core, the
//! `barrier()` step and bulk-correct reducers. Step 4 merges the `repeat()` frontier and turned
//! `bulk.merge` on by default; the multiset oracle
//! ([`assert_bulk_invisible`]) compares every query with merging off and on.
//! The checks of the raw traversers and their bulk (how much a run merges) are the unit tests in
//! `src/traversal/bulk_tests.rs`.

use graphersal::prelude::*;

fn modern() -> Graph {
    GraphSource::tinkerpop_modern()
}

type Source<'b> = GraphTraversalSource<'b, &'b TraversalGraph>;

/// Runs `query` with `bulk.merge` off and on and checks both results are the same multiset.
///
/// The order may differ (merge points emit duplicates together), so the comparison does not
/// sort by `Debug` text: every merged result is matched against an equal unmerged one.
#[cfg(feature = "script")]
fn assert_bulk_invisible<F>(query: F)
where
    F: for<'b> Fn(Source<'b>) -> Source<'b>,
{
    assert_bulk_invisible_on(&modern(), query);
}

/// [`assert_bulk_invisible`] on `graph`. Returns the merged result for further assertions.
#[cfg(feature = "script")]
fn assert_bulk_invisible_on<F>(graph: &Graph, query: F) -> Vec<ElementProperty>
where
    F: for<'b> Fn(Source<'b>) -> Source<'b>,
{
    let lock = graph.read();
    let run = |merge: bool| {
        query(lock.traversal().with("bulk.merge", merge))
            .to_list()
            .expect("query runs")
    };
    let mut unmerged = run(false);
    let merged = run(true);
    assert_same_multiset(&mut unmerged, &merged);
    merged
}

fn assert_same_multiset(unmerged: &mut Vec<ElementProperty>, merged: &[ElementProperty]) {
    assert_eq!(unmerged.len(), merged.len(), "result sizes differ");
    // Sorted renderings compare the multisets in O(n log n) (a position()/swap_remove scan was
    // quadratic and took seconds on 12 288 results).
    let mut left: Vec<String> = unmerged.iter().map(|item| format!("{item:?}")).collect();
    let mut right: Vec<String> = merged.iter().map(|item| format!("{item:?}")).collect();
    left.sort();
    right.sort();
    assert!(
        left == right,
        "merged and unmerged results are different multisets"
    );
    unmerged.clear();
}

/// Three layers of four vertices, complete between neighbouring layers: every vertex of layer
/// two is reached by 4 paths from layer zero, so merging has a lot to do. Names are `n<layer><i>`,
/// ages are `10 * layer + i + 1`.
pub(crate) fn dense_graph() -> TraversalGraph {
    let mut tg = TraversalGraph::new();
    let mut layers: Vec<Vec<_>> = Vec::new();
    for layer in 0..3 {
        let mut vertices = Vec::new();
        for i in 0..4 {
            let mut element = PropertyMap::default();
            element.insert("name", format!("n{layer}{i}").as_str().into());
            element.insert("age", ElementProperty::Number((10 * layer + i + 1).into()));
            vertices.push(
                tg.add_vertex(
                    Some(format!("l{layer}_{i}").into()),
                    vec!["n".into()],
                    element,
                )
                .unwrap(),
            );
        }
        layers.push(vertices);
    }
    for pair in layers.windows(2) {
        for from in &pair[0] {
            for to in &pair[1] {
                tg.add_edge(
                    None,
                    Some("next".into()),
                    *from,
                    *to,
                    PropertyMap::default(),
                )
                .unwrap();
            }
        }
    }
    tg
}

pub(crate) fn dense() -> Graph {
    Graph::new(dense_graph())
}

#[cfg(feature = "script")]
#[test]
fn merge_switch_does_not_change_results() {
    assert_bulk_invisible(|g| g.v(None).out(None).out(None));
    assert_bulk_invisible(|g| g.v(None).values("name"));
    assert_bulk_invisible(|g| g.v(None).repeat(__::out(None)).times(2));
}

#[test]
fn bulk_options_are_accepted() {
    let graph = modern();
    let lock = graph.read();
    let results = lock
        .traversal()
        .with("bulk.merge", true)
        .with("bulk.max_expansion", 5)
        .v(None)
        .to_list()
        .unwrap();
    assert_eq!(results.len(), 6);
}

#[test]
fn invalid_bulk_options_fail_with_invalid_option() {
    let graph = modern();
    let lock = graph.read();
    for (key, value) in [
        ("bulk.max_expansion", ElementProperty::from(0)),
        ("bulk.max_expansion", ElementProperty::from(-1)),
        ("bulk.max_expansion", ElementProperty::from("many")),
        ("bulk.merge", ElementProperty::from("yes")),
        ("bulk.merge", ElementProperty::from(1)),
    ] {
        let err = lock
            .traversal()
            .with(key, value)
            .v(None)
            .to_list()
            .expect_err("invalid option");
        assert!(
            matches!(&*err, TraverserError::InvalidOption { key: k, .. } if k == key),
            "{key}: {err:?}"
        );
    }
}

#[cfg(feature = "script")]
#[test]
fn rhai_with_accepts_and_rejects_bulk_options() {
    use std::sync::Arc;

    let graph = Arc::new(modern());
    let run = |script: &str| -> Result<(), String> {
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .map_err(|e| e.to_string())?;
        let lock = graph.read();
        gts.ast_convert_to_gts(&*lock)
            .to_list()
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    };
    run(r#"g.with("bulk.merge", true).with("bulk.max_expansion", 5).V()"#).unwrap();
    for script in [
        r#"g.with("bulk.max_expansion", 0).V()"#,
        r#"g.with("bulk.merge", "yes").V()"#,
    ] {
        let err = run(script).expect_err(script);
        assert!(
            err.contains("InvalidOption") || err.contains("bulk."),
            "{err}"
        );
    }
}

#[cfg(feature = "script")]
#[test]
fn count_is_bulk_weighted_after_a_barrier() {
    for graph in [modern(), dense()] {
        let counts = |merge: bool| {
            let lock = graph.read();
            lock.traversal()
                .with("bulk.merge", merge)
                .v(None)
                .out(None)
                .out(None)
                .barrier()
                .count()
                .to_list()
                .unwrap()
        };
        assert_eq!(counts(false), counts(true));
        assert_bulk_invisible_on(&graph, |g| {
            g.v(None).both(None).both(None).barrier().count()
        });
        assert_bulk_invisible_on(&graph, |g| {
            g.v(None).both(None).both(None).barrier().out(None).count()
        });
    }
}

#[cfg(feature = "script")]
#[test]
fn reducers_are_bulk_correct() {
    for graph in [modern(), dense()] {
        fn fan<'b>(g: Source<'b>) -> Source<'b> {
            g.v(None).both(None).both(None).barrier().values("age")
        }
        assert_bulk_invisible_on(&graph, |g| fan(g).sum());
        assert_bulk_invisible_on(&graph, |g| fan(g).mean());
        assert_bulk_invisible_on(&graph, |g| fan(g).median());
        assert_bulk_invisible_on(&graph, |g| fan(g).min());
        assert_bulk_invisible_on(&graph, |g| fan(g).max());
        // The values themselves, not only their aggregate.
        assert_bulk_invisible_on(&graph, fan);
    }
}

#[test]
fn weighted_median_matches_the_expanded_one() {
    // Uneven bulks: 11 appears 4 times, 12 once, 13 twice.
    let graph = dense();
    let lock = graph.read();
    let median = |merge: bool| {
        lock.traversal()
            .with("bulk.merge", merge)
            .v(None)
            .out(None)
            .barrier()
            .values("age")
            .median()
            .to_list()
            .unwrap()
    };
    assert_eq!(median(false), median(true));
}

#[test]
fn sum_of_a_merged_stream_multiplies_by_the_bulk() {
    let graph = dense();
    let lock = graph.read();
    // Every layer-one vertex is reached from all four layer-zero vertices: ages 11..=14, 4 times.
    let sum = lock
        .traversal()
        .with("bulk.merge", true)
        .v(None)
        .has("name", "n00")
        .out(None)
        .in_(None)
        .out(None)
        .barrier()
        .values("age")
        .sum()
        .to_list()
        .unwrap();
    // n00 -> 4 layer-one vertices -> each back to 4 layer-zero vertices -> each forward to 4
    // layer-one vertices: 64 traversers, each age 11..=14 appearing 16 times.
    assert_eq!(sum[0].as_i64(), Some(16 * (11 + 12 + 13 + 14)));
}

#[test]
fn integer_sum_overflow_of_a_merged_value_is_the_overflow_error() {
    let mut tg = TraversalGraph::new();
    let mut element = PropertyMap::default();
    element.insert("big", ElementProperty::Number((i64::MAX / 2).into()));
    let a = tg.add_vertex(Some("a".into()), vec![], element).unwrap();
    let b = tg
        .add_vertex(Some("b".into()), vec![], PropertyMap::default())
        .unwrap();
    let c = tg
        .add_vertex(Some("c".into()), vec![], PropertyMap::default())
        .unwrap();
    tg.add_edge(None, Some("e".into()), b, a, PropertyMap::default())
        .unwrap();
    tg.add_edge(None, Some("e".into()), c, a, PropertyMap::default())
        .unwrap();
    tg.add_edge(None, Some("e".into()), a, b, PropertyMap::default())
        .unwrap();
    tg.add_edge(None, Some("e".into()), a, c, PropertyMap::default())
        .unwrap();
    let graph: Graph = Graph::new(tg);
    let lock = graph.read();
    // a is reached three times (a->b->a, a->c->a ... plus the start), merged into bulk 3.
    let err = lock
        .traversal()
        .with("bulk.merge", true)
        .v("a")
        .out(None)
        .out(None)
        .both(None)
        .both(None)
        .barrier()
        .has_property("big")
        .values("big")
        .sum()
        .to_list()
        .expect_err("the merged sum overflows i64");
    assert!(
        err.root_cause()
            .to_string()
            .contains("overflowed the signed 64-bit integer"),
        "{err}"
    );
}

#[test]
fn fold_repeats_merged_values() {
    for graph in [modern(), dense()] {
        let lock = graph.read();
        let fold = |merge: bool| {
            lock.traversal()
                .with("bulk.merge", merge)
                .v(None)
                .both(None)
                .both(None)
                .barrier()
                .values("name")
                .fold()
                .to_list()
                .unwrap()
        };
        let (unmerged, merged) = (fold(false), fold(true));
        let flatten = |rows: Vec<ElementProperty>| match rows.into_iter().next() {
            Some(ElementProperty::Array(items)) => items,
            other => panic!("expected one array, got {other:?}"),
        };
        let mut unmerged = flatten(unmerged);
        assert_same_multiset(&mut unmerged, &flatten(merged));
    }
}

#[test]
fn fold_over_the_expansion_limit_fails_with_a_runnable_help() {
    let graph = dense();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("bulk.merge", true)
        .with("bulk.max_expansion", 5)
        .v(None)
        .both(None)
        .both(None)
        .barrier()
        .fold()
        .to_list()
        .expect_err("the fold needs more than 5 values");
    assert!(
        matches!(
            err.root_cause(),
            TraverserError::BulkExpansionLimit { step, limit: 5, .. } if step == "fold"
        ),
        "{err:?}"
    );
    let help = err.help().expect("help");
    assert!(help.contains("'bulk.max_expansion'"), "{help}");
    assert!(help.contains("count()"), "{help}");
    assert!(help.contains("g.with(\"bulk.max_expansion\""), "{help}");
}

#[test]
fn the_expansion_limit_also_guards_the_terminal_list() {
    let graph = dense();
    let lock = graph.read();
    let err = lock
        .traversal()
        .with("bulk.merge", true)
        .with("bulk.max_expansion", 5)
        .v(None)
        .both(None)
        .both(None)
        .barrier()
        .to_list()
        .expect_err("the list needs more than 5 values");
    assert!(
        matches!(&*err, TraverserError::BulkExpansionLimit { .. }),
        "{err:?}"
    );
    // The same query without the cap, or with a count, is fine.
    assert!(
        lock.traversal()
            .with("bulk.merge", true)
            .v(None)
            .both(None)
            .both(None)
            .barrier()
            .count()
            .to_list()
            .is_ok()
    );
}

#[cfg(feature = "script")]
#[test]
fn range_family_counts_units_not_traversers() {
    for graph in [modern(), dense()] {
        // Sorted names put equal values next to each other, so the selection is deterministic
        // with and without merging.
        fn sorted<'b>(g: Source<'b>) -> Source<'b> {
            g.v(None)
                .both(None)
                .both(None)
                .values("name")
                .order()
                .barrier()
        }
        assert_bulk_invisible_on(&graph, |g| sorted(g).limit(3));
        assert_bulk_invisible_on(&graph, |g| sorted(g).limit(0));
        assert_bulk_invisible_on(&graph, |g| sorted(g).range(1, 4));
        assert_bulk_invisible_on(&graph, |g| sorted(g).range(2, -1));
        assert_bulk_invisible_on(&graph, |g| sorted(g).skip(2));
        assert_bulk_invisible_on(&graph, |g| sorted(g).skip(1000));
        assert_bulk_invisible_on(&graph, |g| sorted(g).tail(2));
        assert_bulk_invisible_on(&graph, |g| sorted(g).tail(1000));
        // Without a deterministic order only the counts must agree.
        for slice in [
            (|g: Source<'_>| g.limit(5)) as fn(Source<'_>) -> Source<'_>,
            |g| g.range(3, 9),
            |g| g.skip(7),
            |g| g.tail(7),
        ] {
            let count = |merge: bool| {
                let lock = graph.read();
                let g = lock
                    .traversal()
                    .with("bulk.merge", merge)
                    .v(None)
                    .both(None)
                    .both(None)
                    .barrier();
                slice(g).to_list().unwrap().len()
            };
            assert_eq!(count(false), count(true));
        }
    }
}

#[cfg(feature = "script")]
#[test]
fn order_and_maps_carry_the_bulk() {
    for graph in [modern(), dense()] {
        assert_bulk_invisible_on(&graph, |g| {
            g.v(None).both(None).both(None).barrier().order().by("name")
        });
        assert_bulk_invisible_on(&graph, |g| {
            g.v(None).both(None).both(None).barrier().values("name")
        });
        assert_bulk_invisible_on(&graph, |g| {
            g.v(None)
                .both(None)
                .both(None)
                .barrier()
                .out(None)
                .values("name")
        });
        assert_bulk_invisible_on(&graph, |g| {
            g.v(None)
                .both(None)
                .both(None)
                .barrier()
                .has_label("person")
                .label()
        });
    }
}

#[test]
fn coin_draws_per_unit_of_bulk() {
    let graph = dense();
    let lock = graph.read();
    let count = |p: f32| {
        lock.traversal()
            .with("bulk.merge", true)
            .v(None)
            .both(None)
            .both(None)
            .barrier()
            .coin(p)
            .count()
            .to_list()
            .unwrap()[0]
            .as_i64()
            .unwrap()
    };
    assert_eq!(count(0.0), 0);
    // Two-hop walks from the 12 vertices: 128 per layer, 384 in all.
    assert_eq!(count(1.0), 384);
    let half = count(0.5);
    assert!((100..=290).contains(&half), "{half}");
}

/// A plan that mutates never merges, so every logical traverser still mutates once.
#[test]
fn mutating_plans_never_merge() {
    let counts = |merge: bool| {
        let mut graph = dense_graph();
        let created = graph
            .traversal_mut()
            .with("bulk.merge", merge)
            .v(None)
            .out(None)
            .barrier()
            .add_v("x")
            .count()
            .to_list()
            .unwrap()[0]
            .as_i64()
            .unwrap();
        let total = graph.traversal().v(None).count().to_list().unwrap()[0]
            .as_i64()
            .unwrap();
        (created, total)
    };
    assert_eq!(counts(false), counts(true));
    // 8 layer-one/two vertices, each reached 4 times through 'out' from the 12 sources.
    assert_eq!(counts(true).0, 32);
    assert_eq!(counts(true).1, 12 + 32);
}

/// A mutation in a nested child disables merging for the whole plan.
#[test]
fn a_mutation_in_a_child_traversal_disables_merging() {
    let created = |merge: bool| {
        let mut graph = dense_graph();
        graph
            .traversal_mut()
            .with("bulk.merge", merge)
            .v(None)
            .out(None)
            .barrier()
            .union(vec![__::add_v("y")])
            .to_list()
            .unwrap();
        graph
            .traversal()
            .v(None)
            .has_label("y")
            .count()
            .to_list()
            .unwrap()[0]
            .as_i64()
            .unwrap()
    };
    assert_eq!(created(false), 32);
    assert_eq!(created(true), 32);
}

#[test]
fn barrier_with_size_validates_and_is_named_in_the_profile() {
    let graph = modern();
    let lock = graph.read();
    let ok = lock
        .traversal()
        .v(None)
        .barrier_with_size(10)
        .count()
        .to_list()
        .unwrap();
    assert_eq!(ok[0].as_i64(), Some(6));
    let err = lock
        .traversal()
        .v(None)
        .barrier_with_size(0)
        .to_list()
        .expect_err("a barrier holds at least one traverser");
    assert!(err.to_string().contains("barrier"), "{err}");

    let profile = lock
        .traversal()
        .with("bulk.merge", true)
        .v(None)
        .both(None)
        .both(None)
        .barrier_with_size(7)
        .profile()
        .unwrap()
        .to_string();
    assert!(profile.contains("barrier(7)"), "{profile}");
    let plain = lock
        .traversal()
        .with("bulk.merge", true)
        .v(None)
        .both(None)
        .both(None)
        .barrier()
        .profile()
        .unwrap();
    let row = plain
        .to_string()
        .lines()
        .find(|line| line.starts_with("barrier()"))
        .map(str::to_string)
        .expect("barrier row");
    let numbers: Vec<u64> = row
        .split_whitespace()
        .filter_map(|w| w.parse().ok())
        .collect();
    // Call, In, Out: merging leaves fewer traversers than went in.
    assert!(numbers[2] < numbers[1], "{row}");
}

#[cfg(feature = "script")]
#[test]
fn nested_barrier_in_a_child_traversal_is_bulk_invisible() {
    for graph in [modern(), dense()] {
        assert_bulk_invisible_on(&graph, |g| {
            g.v(None).where_t(
                __::both(None)
                    .barrier()
                    .both(None)
                    .barrier()
                    .count()
                    .is(P::Gt(3)),
            )
        });
        assert_bulk_invisible_on(&graph, |g| {
            g.v(None).local(
                __::both(None)
                    .both(None)
                    .barrier()
                    .values("name")
                    .order()
                    .limit(2),
            )
        });
    }
}

#[cfg(feature = "script")]
mod dsl {
    use super::*;
    use std::sync::Arc;

    fn run(graph: &Arc<Graph>, script: &str) -> Result<Vec<ElementProperty>, String> {
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .map_err(|e| e.to_string())?;
        let lock = graph.read();
        gts.ast_convert_to_gts(&*lock)
            .to_list()
            .map_err(|e| format!("{e:?}"))
    }

    fn assert_invisible(graph: &Arc<Graph>, tail: &str) {
        let mut off = run(graph, &format!(r#"g.with("bulk.merge", false){tail}"#)).unwrap();
        let on = run(graph, &format!(r#"g.with("bulk.merge", true){tail}"#)).unwrap();
        assert_same_multiset(&mut off, &on);
    }

    #[test]
    fn barrier_forms_run_and_agree() {
        for graph in [Arc::new(modern()), Arc::new(dense())] {
            assert_invisible(&graph, ".V().both().both().barrier().count()");
            assert_invisible(&graph, ".V().both().both().barrier(10).count()");
            assert_invisible(
                &graph,
                ".V().both().both().barrier().values(\"name\").dedup()",
            );
            assert_invisible(
                &graph,
                ".V().where(__.both().barrier().both().count().is(gt(3)))",
            );
            assert_invisible(
                &graph,
                ".V().both().barrier().both().barrier().values(\"age\").sum()",
            );
            assert_invisible(
                &graph,
                ".V().both().both().barrier().values(\"age\").mean()",
            );
            assert_invisible(
                &graph,
                ".V().both().both().barrier().values(\"age\").median()",
            );
            let one = run(&graph, "g.V().barrier().count()").unwrap();
            assert!(one[0].as_i64().unwrap() > 0);
        }
    }

    #[test]
    fn barrier_rejects_bad_sizes_without_panicking() {
        let graph = Arc::new(modern());
        for script in [
            "g.V().barrier(0)",
            "g.V().barrier(-1)",
            "g.V().barrier(4294967296)",
            "g.V().barrier(\"x\")",
            "g.V().barrier(1.5)",
            "g.V().barrier(10, 20)",
        ] {
            let err = run(&graph, script).expect_err(script);
            assert!(!err.is_empty(), "{script}");
        }
        let err = run(&graph, "g.V().barrier(0)").unwrap_err();
        assert!(
            err.contains("barrier(0)") && err.contains("from 1 to"),
            "{err}"
        );
        let err = run(&graph, "g.V().barrier(\"x\")").unwrap_err();
        assert!(err.contains("not supported"), "{err}");
    }

    #[test]
    fn mutating_script_with_a_barrier_creates_one_vertex_per_traverser() {
        let created = |merge: bool| {
            let graph = Arc::new(dense());
            let _ = crate::script_host::eval_value(
                graph.clone(),
                &format!(r#"g.with("bulk.merge", {merge}).V().out().barrier().addV("x").toList()"#),
            )
            .unwrap();
            let rows = run(&graph, r#"g.V().hasLabel("x").count()"#).unwrap();
            rows[0].as_i64().unwrap()
        };
        assert_eq!(created(false), 32);
        assert_eq!(created(true), 32);
    }

    #[test]
    fn dsl_profile_shows_the_merge() {
        let graph = Arc::new(modern());
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(
                &mut scope,
                r#"g.with("bulk.merge", true).V().both().both().barrier()"#,
            )
            .unwrap();
        let lock = graph.read();
        let profile = gts
            .ast_convert_to_gts(&*lock)
            .profile()
            .unwrap()
            .to_string();
        assert!(profile.contains("barrier()"), "{profile}");
    }
}

// ---- Step 3: grouping, side effects and child traversals ----

/// Equality of two results that ignores the order of array items at any depth (merge points emit
/// duplicates together, so value lists inside a `group()` may list them in another order) and of
/// object entries.
#[cfg(feature = "script")]
fn same_unordered(a: &ElementProperty, b: &ElementProperty) -> bool {
    match (a, b) {
        (ElementProperty::Array(x), ElementProperty::Array(y)) => {
            if x.len() != y.len() {
                return false;
            }
            let mut rest: Vec<&ElementProperty> = y.iter().collect();
            x.iter().all(
                |item| match rest.iter().position(|c| same_unordered(item, c)) {
                    Some(i) => {
                        rest.swap_remove(i);
                        true
                    }
                    None => false,
                },
            )
        }
        (ElementProperty::Object(x), ElementProperty::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_unordered(v, w)))
        }
        _ => a == b,
    }
}

#[cfg(feature = "script")]
fn assert_same_deep(unmerged: &[ElementProperty], merged: &[ElementProperty]) {
    assert_eq!(unmerged.len(), merged.len(), "result sizes differ");
    let mut rest: Vec<&ElementProperty> = unmerged.iter().collect();
    for item in merged {
        let i = rest
            .iter()
            .position(|c| same_unordered(item, c))
            .unwrap_or_else(|| panic!("merged result {item:?} missing from the unmerged run"));
        rest.swap_remove(i);
    }
}

#[cfg(feature = "script")]
#[test]
fn group_count_counts_units_not_traversers() {
    let merged = assert_bulk_invisible_on(&modern(), |g| {
        g.v(None)
            .both(None)
            .both(None)
            .barrier()
            .values("name")
            .group_count()
    });
    // The real counts, not 1s: 30 two-hop paths in total.
    let ElementProperty::Object(counts) = &merged[0] else {
        panic!("{merged:?}")
    };
    let total: i64 = counts.values().map(|v| v.as_i64().unwrap()).sum();
    assert_eq!(total, 30);
    assert_eq!(counts.get("marko").and_then(|v| v.as_i64()), Some(7));
    assert_bulk_invisible_on(&dense(), |g| {
        g.v(None).out(None).out(None).barrier().group_count()
    });
}

#[cfg(feature = "script")]
#[test]
fn local_gives_one_result_per_logical_traverser() {
    let merged = assert_bulk_invisible_on(&modern(), |g| {
        g.v(None).in_(None).barrier().local(__::count())
    });
    assert_eq!(merged.len(), 6);
    assert!(merged.iter().all(|v| v.as_i64() == Some(1)));
    assert_bulk_invisible_on(&dense(), |g| {
        g.v(None)
            .out(None)
            .out(None)
            .barrier()
            .local(__::out(None).count())
    });
}

#[test]
fn side_effect_callback_runs_once_per_traverser_with_its_bulk() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    // (calls, sum of bulks)
    let observe = |merge: bool| {
        let calls = Arc::new(AtomicU64::new(0));
        let bulks = Arc::new(AtomicU64::new(0));
        let (c, b) = (calls.clone(), bulks.clone());
        let graph = modern();
        let lock = graph.read();
        lock.traversal()
            .with("bulk.merge", merge)
            .v(None)
            .both(None)
            .both(None)
            .barrier()
            .side_effect(Box::new(move |_, bulk| {
                c.fetch_add(1, Ordering::Relaxed);
                b.fetch_add(bulk, Ordering::Relaxed);
            }))
            .to_list()
            .unwrap();
        (calls.load(Ordering::Relaxed), bulks.load(Ordering::Relaxed))
    };
    assert_eq!(observe(false), (30, 30));
    let (calls, bulks) = observe(true);
    assert_eq!(bulks, 30, "the bulk sum is invisible to merging");
    assert!(calls < 30, "a merged traverser is seen once: {calls} calls");
}

#[cfg(feature = "script")]
mod step3_dsl {
    use super::*;
    use std::sync::Arc;

    fn run(graph: &Arc<Graph>, script: &str) -> Result<Vec<ElementProperty>, String> {
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .map_err(|e| e.to_string())?;
        let lock = graph.read();
        gts.ast_convert_to_gts(&*lock)
            .to_list()
            .map_err(|e| format!("{e:?}"))
    }

    /// Runs `tail` after a merging prefix with merging off and on; the results must agree.
    fn invisible(graph: &Arc<Graph>, tail: &str) -> Vec<ElementProperty> {
        let off = run(graph, &format!(r#"g.with("bulk.merge", false){tail}"#))
            .unwrap_or_else(|e| panic!("{tail}: {e}"));
        let on = run(graph, &format!(r#"g.with("bulk.merge", true){tail}"#))
            .unwrap_or_else(|e| panic!("{tail}: {e}"));
        assert_same_deep(&off, &on);
        on
    }

    const TWO_HOP: &str = ".V().both().both().barrier()";

    fn both_graphs() -> [Arc<Graph>; 2] {
        [Arc::new(modern()), Arc::new(dense())]
    }

    #[test]
    fn grouping_is_bulk_invisible() {
        for graph in both_graphs() {
            for tail in [
                r#".values("name").groupCount()"#,
                r#".groupCount().by("name")"#,
                r#".group().by(T.label)"#,
                r#".group().by(T.label).by("name")"#,
                r#".group().by("name").by(T.id)"#,
                r#".group().by(T.label).by(__.count())"#,
                r#".group().by(T.label).by(__.values("age").sum())"#,
                r#".group().by(T.label).by(__.values("age").fold())"#,
                r#".group().by(T.label).by(__.values("age").mean())"#,
                r#".group().by("name").by(__.values("age").max())"#,
            ] {
                invisible(&graph, &format!("{TWO_HOP}{tail}"));
            }
        }
    }

    #[test]
    fn grouped_values_are_listed_once_per_unit() {
        let graph = Arc::new(modern());
        let out = invisible(
            &graph,
            &format!(r#"{TWO_HOP}.group().by(T.label).by("name")"#),
        );
        let ElementProperty::Object(groups) = &out[0] else {
            panic!("{out:?}")
        };
        let total: usize = groups
            .values()
            .map(|v| match v {
                ElementProperty::Array(items) => items.len(),
                other => panic!("{other:?}"),
            })
            .sum();
        assert_eq!(total, 30);
        let sums = invisible(
            &graph,
            &format!(r#"{TWO_HOP}.group().by(T.label).by(__.count())"#),
        );
        let ElementProperty::Object(counts) = &sums[0] else {
            panic!("{sums:?}")
        };
        assert_eq!(
            counts.values().map(|v| v.as_i64().unwrap()).sum::<i64>(),
            30
        );
    }

    #[test]
    fn side_effects_are_bulk_invisible() {
        for graph in both_graphs() {
            for tail in [
                r#".aggregate("x").cap("x")"#,
                r#".store("x").cap("x")"#,
                r#".aggregate("x").by("name").cap("x")"#,
                r#".local(__.aggregate("x")).cap("x")"#,
                r#".local(__.store("x")).cap("x")"#,
                r#".where(__.aggregate("x").out()).cap("x")"#,
                r#".union(__.aggregate("x"), __.identity()).cap("x")"#,
                r#".tree()"#,
                r#".outE().subgraph("s").cap("s").count()"#,
            ] {
                invisible(&graph, &format!("{TWO_HOP}{tail}"));
            }
        }
    }

    #[test]
    fn a_child_with_side_effects_runs_once_per_unit() {
        let graph = Arc::new(modern());
        let out = invisible(
            &graph,
            &format!(r#"{TWO_HOP}.local(__.aggregate("x")).cap("x")"#),
        );
        let ElementProperty::Array(items) = &out[0] else {
            panic!("{out:?}")
        };
        // 30 logical traversers each ran the child once; TinkerPop would stop at 6 here.
        assert_eq!(items.len(), 30);
        let exists = invisible(
            &graph,
            &format!(r#"{TWO_HOP}.where(__.aggregate("y")).cap("y")"#),
        );
        let ElementProperty::Array(items) = &exists[0] else {
            panic!("{exists:?}")
        };
        assert_eq!(items.len(), 30);
    }

    #[test]
    fn child_traversals_are_bulk_invisible() {
        for graph in both_graphs() {
            for tail in [
                ".local(__.count())",
                ".local(__.out().count())",
                ".local(__.out().values(\"name\").fold())",
                ".union(__.out(), __.in())",
                ".coalesce(__.out(), __.identity())",
                ".optional(__.out())",
                ".choose(__.out(), __.out(), __.in())",
                ".choose(__.out().count().is(gt(1)), __.identity(), __.out())",
                ".where(__.out())",
                ".and(__.out(), __.in())",
                ".or(__.out(), __.in())",
                ".not(__.out())",
                ".project(\"a\").by(__.out().count())",
                ".project(\"a\", \"b\").by(__.out().count()).by(__.values(\"name\"))",
                ".order().by(__.out().count())",
                ".order().by(__.out().count(), Order.desc)",
                ".values(\"age\").math(\"_ * 2\")",
            ] {
                invisible(&graph, &format!("{TWO_HOP}{tail}"));
            }
        }
    }

    #[test]
    fn local_count_on_a_merged_stream_yields_ones() {
        let graph = Arc::new(modern());
        let out = invisible(&graph, ".V().in().barrier().local(__.count())");
        assert_eq!(out.len(), 6);
        assert!(out.iter().all(|v| v.as_i64() == Some(1)));
    }

    #[test]
    fn expansion_limit_guards_aggregate_store_and_group_value_lists() {
        let graph = Arc::new(dense());
        for (tail, step) in [
            (r#".aggregate("x")"#, "aggregate"),
            (r#".store("x")"#, "store"),
            (r#".group().by(T.label).by("name")"#, "group"),
            (r#".values("name").fold()"#, "fold"),
        ] {
            let script = format!(
                r#"g.with("bulk.merge", true).with("bulk.max_expansion", 3).V().out().out().barrier(){tail}"#
            );
            let engine = crate::script_host::engine();
            let mut scope = crate::script_host::graph_scope(graph.clone());
            let gts = engine
                .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(
                    &mut scope, &script,
                )
                .unwrap();
            let lock = graph.read();
            let err = gts.ast_convert_to_gts(&*lock).to_list().expect_err(&script);
            assert!(
                matches!(
                    err.root_cause(),
                    TraverserError::BulkExpansionLimit { step: s, limit: 3, .. } if s == step
                ),
                "{script}: {err:?}"
            );
            let help = err.help().expect("help");
            assert!(help.contains(&format!("Step '{step}'")), "{help}");
            assert!(help.contains("g.with(\"bulk.max_expansion\""), "{help}");
            assert!(help.contains("group_count()"), "{help}");
        }
        // Counting forms never expand, so the same limit does not matter for them.
        for tail in [
            ".groupCount()",
            ".group().by(T.label).by(__.count())",
            ".count()",
        ] {
            let script = format!(
                r#"g.with("bulk.merge", true).with("bulk.max_expansion", 3).V().out().out().barrier(){tail}"#
            );
            run(&graph, &script).unwrap_or_else(|e| panic!("{script}: {e}"));
        }
    }

    #[test]
    fn tree_is_bulk_insensitive() {
        let graph = Arc::new(modern());
        invisible(&graph, ".V().out().barrier().out().tree()");
        invisible(&graph, ".V().out().barrier().out().tree().by(\"name\")");
    }
}

// ---- Step 4: repeat() frontier merge, merging on by default ----

mod repeat_merge {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn repeat_queries_are_bulk_invisible_on_modern_and_dense() {
        for graph in [modern(), dense()] {
            assert_bulk_invisible_on(&graph, |g| g.v(None).repeat(__::out(None)).times(2));
            assert_bulk_invisible_on(&graph, |g| {
                g.v(None).repeat(__::both(None)).times(3).group_count()
            });
            assert_bulk_invisible_on(&graph, |g| g.v(None).emit().repeat(__::out(None)).times(2));
            assert_bulk_invisible_on(&graph, |g| {
                g.v(None)
                    .repeat(__::out(None))
                    .until(__::has("name", "ripple"))
            });
            assert_bulk_invisible_on(&graph, |g| {
                g.v(None).repeat(__::both(None)).times(2).dedup()
            });
            assert_bulk_invisible_on(&graph, |g| g.v(None).repeat(__::out(None)).times(2).path());
            assert_bulk_invisible_on(&graph, |g| {
                g.v(None)
                    .repeat(__::repeat(__::out(None)).times(1))
                    .times(2)
            });
            assert_bulk_invisible_on(&graph, |g| {
                g.v(None)
                    .repeat(__::both(None))
                    .times(3)
                    .values("name")
                    .group_count()
            });
            assert_bulk_invisible_on(&graph, |g| {
                g.with("repeat.order", "dfs")
                    .v(None)
                    .repeat(__::both(None))
                    .times(3)
                    .count()
            });
        }
    }

    #[test]
    fn profile_loop_statistics_do_not_depend_on_merging() {
        let graph = modern();
        let lock = graph.read();
        let render = |merge: bool| {
            lock.traversal()
                .with("bulk.merge", merge)
                .v(None)
                .emit()
                .repeat(__::both(None))
                .times(3)
                .profile()
                .unwrap()
                .to_string()
        };
        let loops = |text: &str| {
            text.lines()
                .find(|l| l.contains("[loops:"))
                .map(str::to_string)
                .unwrap_or_default()
        };
        let (off, on) = (render(false), render(true));
        assert!(loops(&off).contains("avg_loops"), "{off}");
        assert_eq!(loops(&off), loops(&on));
    }

    #[cfg(feature = "io")]
    mod grateful {
        use super::*;

        fn grateful() -> Graph {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/tinkerpop/data/grateful-dead.xml"
            );
            GraphSource::from_graphml(path).expect("the grateful graph loads")
        }

        #[test]
        fn times_eight_count_runs_fast_and_exact() {
            let graph = grateful();
            let lock = graph.read();
            let count = lock
                .traversal()
                .with("evaluationTimeout", 20_000)
                .v(None)
                .repeat(__::out(None))
                .times(8)
                .count()
                .next()
                .unwrap()
                .and_then(|c| c.as_i64());
            assert_eq!(count, Some(2_505_037_961_767_380));
        }

        #[cfg(feature = "script")]
        #[test]
        fn merged_equals_unmerged_for_times_two() {
            let graph = grateful();
            let lock = graph.read();
            let run = |merge: bool| {
                lock.traversal()
                    .with("bulk.merge", merge)
                    .v(None)
                    .repeat(__::out(None))
                    .times(2)
                    .count()
                    .next()
                    .unwrap()
                    .and_then(|c| c.as_i64())
            };
            assert_eq!(run(true), Some(327_370));
            assert_eq!(run(false), Some(327_370));
        }

        #[cfg(feature = "script")]
        #[test]
        fn rhai_form_of_the_grateful_count() {
            let graph = std::sync::Arc::new(grateful());
            let out = crate::script_host::eval_value(
                graph,
                r#"g.with("evaluationTimeout", 20000).V().repeat(__.out()).times(8).count().next()"#,
            )
            .unwrap();
            assert!(format!("{out:?}").contains("2505037961767380"), "{out:?}");
        }
    }
}

// ---- Step 5: lazy_barrier rule, profile Bulk column, adaptive merge skipping ----

mod lazy_barrier {
    use super::*;

    fn profile_of<F>(graph: &Graph, query: F) -> String
    where
        F: for<'b> Fn(Source<'b>) -> Source<'b>,
    {
        let lock = graph.read();
        query(lock.traversal()).profile().unwrap().to_string()
    }

    #[test]
    fn profile_shows_the_inserted_barrier_and_the_rule() {
        let text = profile_of(&dense(), |g| {
            g.v(None).out(None).out(None).out(None).count()
        });
        assert_eq!(text.matches("lazy_barrier()").count(), 2, "{text}");
        assert!(
            text.lines()
                .any(|l| l.starts_with("Optimizer rules applied:") && l.contains("lazy_barrier")),
            "{text}"
        );
    }

    #[test]
    fn disabling_the_rule_removes_the_barrier() {
        let graph = dense();
        let lock = graph.read();
        let text = lock
            .traversal()
            .with("optimizer.disabled", vec!["lazy_barrier"])
            .v(None)
            .out(None)
            .out(None)
            .count()
            .profile()
            .unwrap()
            .to_string();
        assert!(!text.contains("lazy_barrier"), "{text}");
        assert!(!text.contains("Bulk"), "{text}");
    }

    #[cfg(feature = "script")]
    #[test]
    fn results_are_equal_with_and_without_the_rule() {
        fn run<F>(graph: &Graph, query: F)
        where
            F: for<'b> Fn(Source<'b>) -> Source<'b>,
        {
            let lock = graph.read();
            let mut without = query(
                lock.traversal()
                    .with("optimizer.disabled", vec!["lazy_barrier"]),
            )
            .to_list()
            .unwrap();
            let with = query(lock.traversal()).to_list().unwrap();
            assert_same_multiset(&mut without, &with);
        }
        for graph in [modern(), dense()] {
            run(&graph, |g| g.v(None).out(None).out(None));
            run(&graph, |g| {
                g.v(None).both(None).both(None).both(None).group_count()
            });
            run(&graph, |g| g.v(None).out_e(None).in_v().out(None));
            run(&graph, |g| g.v(None).out(None).as_("a").out(None));
            run(&graph, |g| g.v(None).out(None).out(None).path());
            run(&graph, |g| {
                g.v(None).both(None).both(None).values("name").group_count()
            });
            run(&graph, |g| {
                g.v(None).repeat(__::both(None).both(None)).times(2)
            });
            run(&graph, |g| {
                g.v(None)
                    .union(vec![__::out(None).out(None), __::in_(None).in_(None)])
            });
        }
    }

    #[test]
    fn nothing_is_inserted_across_as_or_at_the_end() {
        let text = profile_of(&modern(), |g| {
            g.v(None).out(None).as_("a").out(None).select("a")
        });
        assert!(!text.contains("lazy_barrier"), "{text}");
        let text = profile_of(&modern(), |g| g.v(None).out(None));
        assert!(!text.contains("lazy_barrier"), "{text}");
    }

    #[test]
    fn merging_queries_show_a_bulk_column_and_plain_ones_do_not() {
        let text = profile_of(&dense(), |g| {
            g.v(None).out(None).out(None).out(None).count()
        });
        let header = text.lines().find(|l| l.starts_with("Step")).unwrap();
        assert!(header.contains("Out") && header.contains("Bulk"), "{text}");
        // out(): 32 traversers -> lazy_barrier(): merged to 8 that still stand for all 32.
        let row = text
            .lines()
            .find(|l| l.starts_with("lazy_barrier()"))
            .unwrap();
        let cells: Vec<&str> = row.split_whitespace().collect();
        // name, call, in, out, bulk, time...
        assert_eq!(cells[1..5], ["1", "32", "8", "32"], "{text}");

        let plain = profile_of(&modern(), |g| g.v(None).has_label("person").values("name"));
        let header = plain.lines().find(|l| l.starts_with("Step")).unwrap();
        assert!(!header.contains("Bulk"), "{plain}");
        let words: Vec<&str> = header.split_whitespace().collect();
        assert_eq!(
            words,
            ["Step", "Call", "In", "Out", "Time", "%", "Dur"],
            "{plain}"
        );
    }

    #[test]
    fn repeat_rows_carry_the_logical_counts() {
        let text = profile_of(&dense(), |g| g.v(None).repeat(__::out(None)).times(2));
        let row = text.lines().find(|l| l.starts_with("repeat")).unwrap();
        // 4 merged traversers standing for 64 logical ones.
        assert!(row.split_whitespace().any(|c| c == "64"), "{text}");
        assert!(text.contains("Bulk"), "{text}");
    }

    #[cfg(feature = "script")]
    #[test]
    fn rhai_toggles_the_rule_through_with() {
        use std::sync::Arc;
        let graph = Arc::new(modern());
        let run = |script: &str| -> Vec<ElementProperty> {
            let engine = crate::script_host::engine();
            let mut scope = crate::script_host::graph_scope(graph.clone());
            let gts = engine
                .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(
                    &mut scope, script,
                )
                .unwrap();
            let lock = graph.read();
            gts.ast_convert_to_gts(&*lock).to_list().unwrap()
        };
        let mut off = run(
            r#"g.with("optimizer.disabled", ["lazy_barrier"]).V().both().both().values("name")"#,
        );
        let on = run(
            r#"g.with("optimizer.enabled", ["lazy_barrier"]).V().both().both().values("name")"#,
        );
        assert_same_multiset(&mut off, &on);

        let lock = graph.read();
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(
                &mut scope,
                r#"g.with("optimizer.disabled", ["lazy_barrier"]).V().out().out().count()"#,
            )
            .unwrap();
        let text = gts
            .ast_convert_to_gts(&*lock)
            .profile()
            .unwrap()
            .to_string();
        assert!(!text.contains("lazy_barrier"), "{text}");
    }
}

/// Adaptive skipping: a merge pass that finds almost nothing to merge stops early; it never
/// changes results.
mod adaptive_skip {
    use super::*;

    /// `n` sources, each with one edge to its own sink (nothing converges).
    fn sparse(n: usize) -> Graph {
        let mut tg = TraversalGraph::new();
        for i in 0..n {
            let a = tg
                .add_vertex(
                    Some(format!("a{i}").into()),
                    vec!["a".into()],
                    PropertyMap::default(),
                )
                .unwrap();
            let b = tg
                .add_vertex(
                    Some(format!("b{i}").into()),
                    vec!["b".into()],
                    PropertyMap::default(),
                )
                .unwrap();
            let c = tg
                .add_vertex(
                    Some(format!("c{i}").into()),
                    vec!["c".into()],
                    PropertyMap::default(),
                )
                .unwrap();
            tg.add_edge(None, Some("e".into()), a, b, PropertyMap::default())
                .unwrap();
            tg.add_edge(None, Some("e".into()), b, c, PropertyMap::default())
                .unwrap();
        }
        Graph::new(tg)
    }

    /// `n` sources that all point at one hub, which points at one sink.
    #[cfg(feature = "script")]
    fn star(n: usize) -> Graph {
        let mut tg = TraversalGraph::new();
        let hub = tg
            .add_vertex(
                Some("hub".into()),
                vec!["hub".into()],
                PropertyMap::default(),
            )
            .unwrap();
        let sink = tg
            .add_vertex(
                Some("sink".into()),
                vec!["sink".into()],
                PropertyMap::default(),
            )
            .unwrap();
        tg.add_edge(None, Some("e".into()), hub, sink, PropertyMap::default())
            .unwrap();
        for i in 0..n {
            let a = tg
                .add_vertex(
                    Some(format!("a{i}").into()),
                    vec!["a".into()],
                    PropertyMap::default(),
                )
                .unwrap();
            tg.add_edge(None, Some("e".into()), a, hub, PropertyMap::default())
                .unwrap();
        }
        Graph::new(tg)
    }

    /// `n` sources that all point at one hub without out-edges.
    #[cfg(feature = "script")]
    fn dead_end(n: usize) -> Graph {
        let mut tg = TraversalGraph::new();
        let hub = tg
            .add_vertex(
                Some("hub".into()),
                vec!["hub".into()],
                PropertyMap::default(),
            )
            .unwrap();
        for i in 0..n {
            let a = tg
                .add_vertex(
                    Some(format!("a{i}").into()),
                    vec!["a".into()],
                    PropertyMap::default(),
                )
                .unwrap();
            tg.add_edge(None, Some("e".into()), a, hub, PropertyMap::default())
                .unwrap();
        }
        Graph::new(tg)
    }

    /// The `lazy_barrier()` row of a profile: (in, out).
    #[cfg(feature = "script")]
    fn barrier_in_out(graph: &Graph) -> (u64, u64) {
        let lock = graph.read();
        let metrics = lock
            .traversal()
            .v(None)
            .has_label("a")
            .out(None)
            .out(None)
            .count()
            .profile()
            .unwrap();
        let row = metrics
            .metrics()
            .iter()
            .find(|m| m.name() == "lazy_barrier()")
            .expect("the rule inserted a barrier");
        (row.count_in(), row.counts().traverser_count())
    }

    #[cfg(feature = "script")]
    #[test]
    fn duplicates_the_next_step_does_not_expand_are_not_merged() {
        // 6000 traversers on one hub merge into one, but the hub has no out-edges: the following
        // out() does no work for the duplicates, so the merge would only cost.
        assert_eq!(barrier_in_out(&dead_end(6000)), (6000, 6000));
        // The same convergence with a hub of fan-out 1 merges: every later step works once.
        assert_eq!(barrier_in_out(&star(6000)), (6000, 1));
        assert_bulk_invisible_on(&dead_end(5000), |g| g.v(None).out(None).out(None).count());
    }

    #[cfg(feature = "script")]
    #[test]
    fn adaptive_skipping_is_bulk_invisible() {
        for graph in [sparse(5000), star(5000)] {
            assert_bulk_invisible_on(&graph, |g| g.v(None).out(None).out(None).count());
            assert_bulk_invisible_on(&graph, |g| g.v(None).repeat(__::out(None)).times(2).count());
            assert_bulk_invisible_on(&graph, |g| {
                g.v(None)
                    .emit()
                    .repeat(__::out(None))
                    .times(3)
                    .group_count()
            });
        }
        // `both().both()` over a star holds ~25M unmerged traversers; the sparse graph keeps
        // the point (a stream above the adaptive probe size) at a fraction of the cost.
        let graph = sparse(5000);
        assert_bulk_invisible_on(&graph, |g| g.v(None).both(None).both(None).count());
    }

    #[test]
    fn a_repeat_stops_merging_after_a_sparse_probe_but_keeps_its_result() {
        let graph = sparse(5000);
        let lock = graph.read();
        let count = lock
            .traversal()
            .v(None)
            .repeat(__::out(None))
            .times(2)
            .count()
            .next()
            .unwrap()
            .and_then(|c| c.as_i64());
        assert_eq!(count, Some(5000));
    }
}
