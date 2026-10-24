use graphersal::prelude::By::*;

// See this crate's `[features] alloc-tracking` in Cargo.toml — bench-only, not part of any
// shipped binary. Installed exactly as `graphersal`/`graphersal-playground` install it, so the
// `_unprofiled` benches below measure the real, always-on per-allocation tax D4/D8 describe.
#[cfg(feature = "alloc-tracking")]
#[global_allocator]
static ALLOC: graphersal::alloc_tracking::TrackingAllocator =
    graphersal::alloc_tracking::TrackingAllocator::new();

fn main() {
    divan::main();
}

#[divan::bench(args = [110_000])]
fn test_graph_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(None).count().to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_get_3_vertex(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(vec!["V_1_1", "V_2_5", "V_1_4"]).count().to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_label(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(None).label().to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_has_label(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(None).has_label("wrong label").to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_has_label_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(None).has_label("wrong label").count().to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_3_node_has_label_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g
                .v(vec!["V_1_1", "V_2_5", "V_1_4"])
                .has_label("wrong label")
                .count()
                .to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_has_age_key_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(None).has_label("age").count().to_list();
        });
}

// Property reads by plain name versus by `JsonPath` key (jpath Task 5). The plain-name pair must
// not move when the path machinery changes; the path pair shows what a path key costs.
#[divan::bench(args = [110_000])]
fn test_graph_has_property_value_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).has("level", "1").count().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_has_property_value_count_jpath(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();
    let path: graphersal::prelude::JsonPath = "level".parse().unwrap();
    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).has(path.clone(), "1").count().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_values_property_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).values("level").count().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_values_property_count_jpath(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();
    let path: graphersal::prelude::JsonPath = "level".parse().unwrap();
    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).values(path.clone()).count().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_where_has(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g
                .v(None)
                .where_t(graphersal::prelude::__::has_label("id"))
                .to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_where_has_as(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g
                .v(None)
                .where_t(graphersal::prelude::__::has_label("id"))
                .as_("v")
                .to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_where_has_path(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g
                .v(None)
                .where_t(graphersal::prelude::__::has_label("id"))
                .path()
                .to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_edge_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.e(None).count().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_edge_has_label_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.e(None).has_label("wrong label").count().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_group(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(None).group().by(Label).to_list();
        });
}

/// `aggregate()`, per `book/src/for_developers/release_plan.md` ("Benchmarks"): a representative shape for the new
/// `aggregate(sideEffectKey)`/`store(sideEffectKey)` side-effect steps — every vertex of the 110k-vertex graph
/// collected into one growing side-effect bucket, then read back with `cap()`.
#[divan::bench(args = [110_000])]
fn test_graph_aggregate(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).aggregate("all").cap("all").to_list();
    });
}

/// `tree()`, per `book/src/for_developers/release_plan.md` ("Benchmarks"): a representative shape for the new
/// `tree()` step — `large_110k` is itself a 10-way
/// branching tree, 5 levels deep (10 + 100 + 1,000 + 10,000 + 100,000 vertices), so walking it
/// from its 10 roots and merging every path back into one hierarchical tree reconstructs the
/// entire 111,110-vertex structure, exercising `tree()`'s merge logic at the scale it exists for.
#[divan::bench(args = [110_000])]
fn test_graph_tree(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .has("level", "0")
            .repeat(graphersal::prelude::__::out(None))
            .times(4)
            .tree(None::<()>)
            .to_list();
    });
}

/// `order()`: a representative shape per
/// `book/src/for_developers/release_plan.md` ("Benchmarks") — sorting the full 110k-vertex stream by a property
/// key (`.by("level")`), the case most likely to regress if the stable `Vec::sort_by` were ever
/// swapped for something that re-resolves the key per comparison instead of once per traverser.
#[divan::bench(args = [110_000])]
fn test_graph_order_by_property(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher
        // .counter(divan::counter::BytesCount::new(args))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(None).order().by("level").count().to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_out_out_path(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).path().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_as_out_out_select(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).as_("a").out(None).out(None).select("a").to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_out_out(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_out_out_simple_path(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).simple_path().to_list();
    });
}

/// Baseline for the `evaluationTimeout` overhead: the same fan-out without a deadline.
#[divan::bench(args = [110_000])]
fn test_graph_out_out_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).count().next();
    });
}

/// The same fan-out under a deadline that never fires: the difference to
/// `test_graph_out_out_count` is the cost of the cooperative timeout checks.
#[divan::bench(args = [110_000])]
fn test_graph_out_out_count_with_timeout(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal().with("evaluationTimeout", 3_600_000);
        let _ = g.v(None).out(None).out(None).count().next();
    });
}

/// `repeat()` baseline: three breadth-first iterations over every vertex.
#[divan::bench(args = [110_000])]
fn test_graph_repeat_out_times_3_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None))
            .times(3)
            .count()
            .next();
    });
}

/// `repeat()` that also emits every visited traverser.
#[divan::bench(args = [110_000])]
fn test_graph_emit_repeat_out_times_3_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .emit()
            .repeat(graphersal::prelude::__::out(None))
            .times(3)
            .count()
            .next();
    });
}

/// `repeat()` whose body reads full paths, so every iteration records them.
#[divan::bench(args = [110_000])]
fn test_graph_repeat_out_simple_path_times_3_count(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None).simple_path())
            .times(3)
            .count()
            .next();
    });
}

/// The `repeat()` baselines in depth-first order, which runs the body once per traverser.
#[divan::bench(args = [110_000])]
fn test_graph_repeat_out_times_3_count_dfs(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal().with("repeat.order", "dfs");
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None))
            .times(3)
            .count()
            .next();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_emit_repeat_out_times_3_count_dfs(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal().with("repeat.order", "dfs");
        let _ = g
            .v(None)
            .emit()
            .repeat(graphersal::prelude::__::out(None))
            .times(3)
            .count()
            .next();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_repeat_out_simple_path_times_3_count_dfs(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal().with("repeat.order", "dfs");
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None).simple_path())
            .times(3)
            .count()
            .next();
    });
}

// --- Memory-inspection spec (D8): unprofiled vs. profiled cost, and `memory_usage()`'s own cost.
//
// Each representative query below is benched twice: once exactly as today (unprofiled — the
// number to compare against this series' prior specs' recorded baselines, e.g. `out_out` ~27-33ms
// and `label` ~5ms on `large`), once with `.profile()` appended, so a report can show the
// profiling path's added cost next to the proof that the unprofiled path did not regress.

#[divan::bench(args = [110_000])]
fn test_graph_out_out_unprofiled(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).to_list();
    });
}

/// `GraphStepFlatMap::process()` buffer-reuse spec (D6): the same high-fan-out `out().out()`
/// chain as `test_graph_out_out_unprofiled`, deliberately without `.path()` (path tracking is
/// covered separately by `path_tests.rs`), reporting *tracked* allocation bytes under
/// `alloc-tracking` via divan's `BytesCount` counter, so `cargo bench -p graphersal --bench graph --features io,script,display,serde,alloc-tracking` surfaces a
/// regression that reintroduces a per-`process()`-call allocation as a jump in reported
/// bytes/iteration throughput, not just as a wall-clock blip. The counter's value is captured
/// once, from one same-shaped dry run using this module's `TrackingAllocator` (see the top of
/// this file), since the query's tracked byte cost is intrinsic to the executor path rather than
/// something the timing harness varies per iteration.
#[cfg(feature = "alloc-tracking")]
#[divan::bench(args = [110_000])]
fn test_graph_out_out_tracked_bytes(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    let before = graphersal::alloc_tracking::thread_allocated_bytes();
    {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).to_list();
    }
    let tracked_bytes = graphersal::alloc_tracking::thread_allocated_bytes().saturating_sub(before);

    bencher
        .counter(divan::counter::BytesCount::new(tracked_bytes))
        .bench(|| {
            let g = lock.traversal();
            let _ = g.v(None).out(None).out(None).to_list();
        });
}

#[divan::bench(args = [110_000])]
fn test_graph_out_out_profiled(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).profile();
    });
}

/// `ProfileType` bitmask spec (D6): the same `out().out()` chain, but with
/// `.profile_with(ProfileType::Memory)` — memory collection explicitly requested. Compared
/// against `test_graph_out_out_profiled` (memory now off by default) in an `alloc-tracking`-
/// enabled build, this is the "pay only when asked" claim's actual cost side: how much more
/// `AllocSnapshot::capture`/`step_memory_metrics` add per call once turned back on.
#[divan::bench(args = [110_000])]
fn test_graph_out_out_profiled_with_memory(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .out(None)
            .out(None)
            .profile_with(graphersal::profile::ProfileType::Memory);
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_label_unprofiled(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).label().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_label_profiled(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).label().profile();
    });
}

/// `ProfileType` bitmask spec (D6): see `test_graph_out_out_profiled_with_memory`'s doc comment.
#[divan::bench(args = [110_000])]
fn test_graph_label_profiled_with_memory(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .label()
            .profile_with(graphersal::profile::ProfileType::Memory);
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_count_unprofiled(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).count().to_list();
    });
}

#[divan::bench(args = [110_000])]
fn test_graph_count_profiled(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).count().profile();
    });
}

/// `ProfileType` bitmask spec (D6): see `test_graph_out_out_profiled_with_memory`'s doc comment.
#[divan::bench(args = [110_000])]
fn test_graph_count_profiled_with_memory(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .count()
            .profile_with(graphersal::profile::ProfileType::Memory);
    });
}

/// D1's O(n) property walk, wall-clock, on the `large` (~110k element) fixture — informational,
/// no hard threshold.
#[divan::bench(args = [110_000])]
fn test_graph_memory_usage(bencher: divan::Bencher, _args: usize) {
    use graphersal::storage::GraphStorage;
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let _ = lock.memory_usage();
    });
}

/// Regression guard for an O(n)-bucket-scan `drop()` defect: `GraphIndex::remove_node_label`
/// used to `Vec::retain` over the *entire* label bucket on every single drop, so dropping a
/// fraction of a large label's members cost O(dropped × bucket_size), not O(dropped). This
/// bench's own fixture isn't `GraphSource::large_110k()` (that fixture gives every vertex its
/// own unique per-vertex label, so it has no large shared label bucket to reproduce the defect
/// against) — instead it builds a bucket at the same order of magnitude as the audit's
/// audit dataset's "company" label (88,000 members) directly via `insert_vertex`, which
/// is the shape that actually exercises this bug. `with_inputs` builds a fresh graph per
/// iteration (untimed) since `drop()` is a mutation — the timed closure only ever measures the
/// drop itself. A future regression of this exact O(n²)-class shape should show up here as a
/// large jump in the per-iteration time, not just as another accidental multi-second discovery
/// against a real bench-scale graph.
#[divan::bench(args = [20_000])]
fn test_graph_drop_large_label_bucket(bencher: divan::Bencher, args: usize) {
    const BUCKET_SIZE: usize = 88_000;

    bencher
        .with_inputs(|| {
            let mut graph = graphersal::TraversalGraph::new();
            for _ in 0..BUCKET_SIZE {
                graph
                    .insert_vertex(
                        None,
                        vec!["bulk".into()],
                        graphersal::prelude::PropertyMap::default(),
                    )
                    .expect("add_vertex should succeed");
            }
            graph
        })
        .bench_local_values(|mut graph| {
            let _ = graph
                .traversal_mut()
                .v(None)
                .has_label("bulk")
                .limit(args)
                .drop()
                .to_list();
        });
}

/// `bothV()`, a representative
/// shape per `book/src/for_developers/release_plan.md` ("Benchmarks"): both endpoints of every outgoing edge of
/// the full 110k-vertex stream (twice the edge count in traversers out). `bothV()` needs no path
/// lookback at all — see `test_graph_out_e_in_v` for the equal-cardinality, no-path baseline
/// `test_graph_other_v` below is measured against.
#[divan::bench(args = [110_000])]
fn test_graph_both_v(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out_e(None).both_v().to_list();
    });
}

/// Baseline for `test_graph_other_v`: same `v().out_e()` fan-out and the same one-traverser-per-
/// edge cardinality as `otherV()`, but through `in_v()`, which needs no path at all. The delta
/// between this and `test_graph_other_v` isolates `otherV()`'s actual, measured
/// `PathRequirement::Full` path-tracking cost (D1) from the cost of the fan-out itself, which
/// `bothV()`'s doubled output count would otherwise confound.
#[divan::bench(args = [110_000])]
fn test_graph_out_e_in_v(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out_e(None).in_v().to_list();
    });
}

/// `otherV()`'s path-lookback cost (D1 of the same spec): `otherV()` declares
/// `PathRequirement::Full`, so every step upstream of it — here just `v()`/`out_e()` — pays for
/// recording its output into the path arena, unlike the label-free, path-free steps everywhere
/// else in this file. See `test_graph_out_e_in_v` above for the equal-cardinality baseline this
/// is measured against.
#[divan::bench(args = [110_000])]
fn test_graph_other_v(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out_e(None).other_v().to_list();
    });
}

/// `range(low, high)`, per
/// `book/src/for_developers/release_plan.md` ("Benchmarks") — a representative pagination shape: skipping the
/// first 100 of the full 110k-vertex stream and taking the next 1000. Guards against a future
/// change that turns the skip-then-take `Iterator` chain into a full buffer-then-slice
/// materialization (D1's lazy-evaluation goal, mirroring `limit()`'s own precedent).
#[divan::bench(args = [110_000])]
fn test_graph_range(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).range(100, 1_100).count().to_list();
    });
}

/// `properties()`, per
/// `book/src/for_developers/release_plan.md` ("Benchmarks") `properties()` is lazy (`materializes() == false`,
/// like `values()`): each of the three stored properties (`id`/`label`/`level`) on every one of
/// the 110k vertices becomes one `VertexProperty` reference, without resolving to a scalar until
/// `to_list()`'s own terminal `into_node_property`.
#[divan::bench(args = [110_000])]
fn test_graph_properties(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).properties(None).count().to_list();
    });
}

/// `properties(key)` with named keys, the other half of `properties()`'s arg handling (see
/// `test_graph_properties` above for the no-args shape).
#[divan::bench(args = [110_000])]
fn test_graph_properties_named(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).properties(vec!["id", "level"]).count().to_list();
    });
}

/// `valueMap()`, `properties()`'s materializing sibling (`materializes() == true`, like
/// `element_map()`): builds a key -> value-list `Record` per vertex (each value wrapped in a
/// one-element list, as in TinkerPop), excluding `id`/`label` (unlike `element_map()`), for all
/// three of `large_110k`'s stored properties.
#[divan::bench(args = [110_000])]
fn test_graph_value_map(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).value_map(None).to_list();
    });
}

/// `local(traversal)`: runs `__.out(None).limit(2)` once per incoming vertex, in isolation, so
/// each vertex contributes at most 2 of its own out-neighbours instead of the whole flattened
/// stream being capped at 2 total. Representative shape for this lazy `FlatMap` step, per
/// `book/src/for_developers/release_plan.md` ("Benchmarks").
#[divan::bench(args = [110_000])]
fn test_graph_local(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .local(graphersal::prelude::__::out(None).limit(2))
            .to_list();
    });
}

/// `choose(predicate, onTrue, onFalse)`: the 3-argument if/then/else sugar form, representative
/// of `choose()`/`branch()`'s shared selection mechanism (`step_choose.rs`'s `StepBranch`). Per
/// `book/src/for_developers/release_plan.md` ("Benchmarks").
#[divan::bench(args = [110_000])]
fn test_graph_choose(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();

    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .choose(
                graphersal::prelude::__::has("level", "0"),
                graphersal::prelude::__::out(None).limit(2),
                graphersal::prelude::__::identity(),
            )
            .to_list();
    });
}

/// Fan-out of the `glob_path` bench tree: subdirectories per directory.
const TREE_FAN_OUT: usize = 4;
/// Files per directory of the `glob_path` bench tree.
const TREE_FILES: usize = 10;
/// Directory levels of the `glob_path` bench tree, the root included.
const TREE_DEPTH: usize = 9;

/// A deterministic directory tree of 961,191 vertices for the `glob_path` benches, built once
/// and shared: `TREE_DEPTH` levels of `directory` vertices named `d0`..`d3` (`TREE_FAN_OUT`
/// each), `TREE_FILES` `file` vertices per directory named `f<i>.rs` / `.md` / `.txt` in turn
/// (so 4 of every 10 files are `.rs`), `name` on every vertex, `contains` edges parent -> child.
/// The root has id `root`; every other id is a sequence number.
fn glob_tree() -> &'static graphersal::TraversalGraph {
    static TREE: std::sync::OnceLock<graphersal::TraversalGraph> = std::sync::OnceLock::new();
    TREE.get_or_init(|| {
        const EXTENSIONS: [&str; 3] = ["rs", "md", "txt"];
        let mut graph = graphersal::TraversalGraph::new();
        let mut next_id = 0usize;
        let mut vertex =
            |graph: &mut graphersal::TraversalGraph, id: Option<&str>, label: &str, name: &str| {
                let mut props = graphersal::prelude::PropertyMap::default();
                props.insert("name", name.into());
                let id = id.map_or_else(
                    || {
                        next_id += 1;
                        next_id.to_string()
                    },
                    str::to_string,
                );
                graph
                    .insert_vertex(Some(id.into()), vec![label.into()], props)
                    .unwrap()
            };
        let root = vertex(&mut graph, Some("root"), "directory", "root");
        let mut level = vec![root];
        for depth in 0..TREE_DEPTH {
            let mut next = Vec::with_capacity(level.len() * TREE_FAN_OUT);
            for &parent in &level {
                for f in 0..TREE_FILES {
                    let name = format!("f{f}.{}", EXTENSIONS[f % EXTENSIONS.len()]);
                    let file = vertex(&mut graph, None, "file", &name);
                    graph
                        .insert_edge(
                            None,
                            Some("contains".into()),
                            parent,
                            file,
                            Default::default(),
                        )
                        .unwrap();
                }
                if depth + 1 == TREE_DEPTH {
                    continue;
                }
                for s in 0..TREE_FAN_OUT {
                    let dir = vertex(&mut graph, None, "directory", &format!("d{s}"));
                    graph
                        .insert_edge(
                            None,
                            Some("contains".into()),
                            parent,
                            dir,
                            Default::default(),
                        )
                        .unwrap();
                    next.push(dir);
                }
            }
            level = next;
        }
        graph
    })
}

/// `glob_path("**/*.rs").by("name").to_list()`: the full walk of the ~1M-vertex tree with every
/// `.rs` file (349,524) materialized as a traverser.
#[divan::bench]
fn test_graph_glob_path_to_list(bencher: divan::Bencher) {
    let graph = glob_tree();
    bencher.bench(|| {
        let _ = graph
            .traversal()
            .v("root")
            .glob_path("**/*.rs")
            .by("name")
            .to_list();
    });
}

/// `glob_path("**/*.rs").by("name").count()`: the same walk through the step's `count()` hook,
/// which counts matches without creating traversers.
#[divan::bench]
fn test_graph_glob_path_count(bencher: divan::Bencher) {
    let graph = glob_tree();
    bencher.bench(|| {
        let _ = graph
            .traversal()
            .v("root")
            .glob_path("**/*.rs")
            .by("name")
            .count()
            .to_list();
    });
}

/// `glob_path("**/*.rs").by("name").by(["directory","file"]).count()`: the cost of the
/// vertex-label filter on top of the counting walk.
#[divan::bench]
fn test_graph_glob_path_label_filter_count(bencher: divan::Bencher) {
    let graph = glob_tree();
    bencher.bench(|| {
        let _ = graph
            .traversal()
            .v("root")
            .glob_path("**/*.rs")
            .by("name")
            .by(["directory", "file"])
            .count()
            .to_list();
    });
}

/// A deep literal path down the ~1M-vertex tree (one match): each level keeps one child, so the
/// pruning reads only the children (at most 14) of the 9 directories on the path.
#[divan::bench]
fn test_graph_glob_path_deep_literal(bencher: divan::Bencher) {
    let graph = glob_tree();
    bencher.bench(|| {
        let _ = graph
            .traversal()
            .v("root")
            .glob_path("d0/d1/d2/d3/d0/d1/d2/d3/f0.rs")
            .by("name")
            .to_list();
    });
}

/// `glob_path("**/*.rs", #{max_depth: 4}).by("name").count()`: the depth bound cuts the walk of
/// the 9-level tree after 4 levels (85 directories expanded instead of all 87,381).
#[divan::bench]
fn test_graph_glob_path_max_depth_count(bencher: divan::Bencher) {
    let graph = glob_tree();
    bencher.bench(|| {
        let _ = graph
            .traversal()
            .v("root")
            .glob_path_with(
                "**/*.rs",
                graphersal::exec::GlobPathOptions::default().max_depth(4),
            )
            .by("name")
            .count()
            .to_list();
    });
}

/// `glob_path("**/*.rs", #{prune: __.has("name", "d0").in().has_id("root")}).by("name").count()`:
/// the prune child runs once per expanded directory (and file) and cuts the `d0` subtree below the
/// root, a quarter of the tree, so this measures the per-vertex cost of the child traversal
/// against the work it saves (compare `test_graph_glob_path_count`).
#[divan::bench]
fn test_graph_glob_path_prune_count(bencher: divan::Bencher) {
    let graph = glob_tree();
    bencher.bench(|| {
        let prune = graphersal::prelude::__::has("name", "d0")
            .in_(None)
            .has_id("root");
        let _ = graph
            .traversal()
            .v("root")
            .glob_path_with(
                "**/*.rs",
                graphersal::exec::GlobPathOptions::default().prune(prune),
            )
            .by("name")
            .count()
            .to_list();
    });
}

fn grateful() -> graphersal::prelude::Graph {
    graphersal::GraphSource::from_graphml(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/tinkerpop/data/grateful-dead.xml"
    ))
    .expect("grateful-dead.xml loads")
}

/// `repeat(out()).times(n).count()` on the grateful graph, where paths converge on few vertices:
/// the frontier merge turns an exponential traverser count into a bounded one.
#[divan::bench(args = [3, 8])]
fn repeat_out_times_count_grateful(bencher: divan::Bencher, n: u32) {
    let graph = grateful();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None))
            .times(n)
            .count()
            .next();
    });
}

/// Same with `bulk.merge` off: the cost without merging (3 iterations only, 8 would not finish).
#[divan::bench(args = [3])]
fn repeat_out_times_count_grateful_unmerged(bencher: divan::Bencher, n: u32) {
    let graph = grateful();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal().with("bulk.merge", false);
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None))
            .times(n)
            .count()
            .next();
    });
}

/// The same repeat with `withSack(0)`: equal sacks share a handle and keep merging, so the time
/// stays within noise of the sackless query.
#[divan::bench(args = [3, 8])]
fn repeat_out_times_count_grateful_sack(bencher: divan::Bencher, n: u32) {
    let graph = grateful();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal().with_sack(0);
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None))
            .times(n)
            .count()
            .next();
    });
}

/// A sack merge operator: the automatic merge points are off, so the loop body merges with an
/// explicit `barrier()` and combines the sacks with `sum`.
#[divan::bench(args = [3])]
fn repeat_out_barrier_times_count_grateful_sack_merge(bencher: divan::Bencher, n: u32) {
    let graph = grateful();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock
            .traversal()
            .with_sack_merge(1, graphersal::prelude::Operator::Sum);
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None).barrier())
            .times(n)
            .count()
            .next();
    });
}

/// Merge overhead measurement: the 110k graph is tree-shaped, so almost nothing merges.
#[divan::bench(args = [3])]
fn repeat_out_times_count_large_merged(bencher: divan::Bencher, n: u32) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None))
            .times(n)
            .count()
            .next();
    });
}

/// See [`repeat_out_times_count_large_merged`]: the same query with merging off.
#[divan::bench(args = [3])]
fn repeat_out_times_count_large_unmerged(bencher: divan::Bencher, n: u32) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal().with("bulk.merge", false);
        let _ = g
            .v(None)
            .repeat(graphersal::prelude::__::out(None))
            .times(n)
            .count()
            .next();
    });
}

/// `out().out().out().count()` on the grateful graph with the `lazy_barrier` rule.
#[divan::bench]
fn out_out_out_count_grateful_merged(bencher: divan::Bencher) {
    let graph = grateful();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).out(None).count().next();
    });
}

/// The same query with `lazy_barrier` disabled.
#[divan::bench]
fn out_out_out_count_grateful_unmerged(bencher: divan::Bencher) {
    let graph = grateful();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock
            .traversal()
            .with("optimizer.disabled", vec!["lazy_barrier"]);
        let _ = g.v(None).out(None).out(None).out(None).count().next();
    });
}

/// Merge overhead of `lazy_barrier` on the tree-shaped 110k graph (`out().out()` with a terminal
/// list, so nothing fuses into a count).
#[divan::bench(args = [110_000])]
fn out_out_large_lazy_barrier(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock.traversal();
        let _ = g.v(None).out(None).out(None).to_list();
    });
}

/// A deterministic pseudo-random embedding in -1..1.
fn embedding(seed: u64, dimension: usize) -> Vec<f32> {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..dimension)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 40) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0) as f32
        })
        .collect()
}

/// 10 000 vertices `doc` with a 384-dimensional random embedding `emb` stored as `element`.
fn vector_docs(element: &str) -> graphersal::prelude::Graph {
    use graphersal::prelude::*;
    let kind = graphersal::vector::VectorElement::from_name(element).unwrap();
    let mut graph = TraversalGraph::new();
    for i in 0..10_000u64 {
        let emb = Vector::from_f32(embedding(i, 384))
            .unwrap()
            .quantize(kind)
            .unwrap();
        let mut props = PropertyMap::default();
        props.insert("emb", ElementProperty::Vector(emb));
        graph.add_vertex(None, vec!["doc".into()], props).unwrap();
    }
    Graph::new(graph)
}

/// Exact `nearest(.., 10)` over 10k x 384 vectors of each element kind (cosine; hamming for bit;
/// the query is the f32 embedding, quantized for bit). The book page "Vectors" quotes it.
#[divan::bench(args = ["f32", "f16", "i8", "bit"])]
fn nearest_10k_x_384(bencher: divan::Bencher, element: &str) {
    use graphersal::prelude::*;
    use graphersal::vector::{VectorElement, VectorMetric};
    let graph = vector_docs(element);
    let lock = graph.read();
    let mut q = Vector::from_f32(embedding(1_000_000, 384)).unwrap();
    let mut metric = None;
    if element == "bit" {
        q = q.quantize(VectorElement::Bit).unwrap();
        metric = Some(VectorMetric::Hamming);
    }
    bencher.bench(|| {
        let _ = lock
            .traversal()
            .v(None)
            .has_label("doc")
            .nearest("emb", &q, 10, metric)
            .to_list();
    });
}

/// The built-in embedder `hash-ngram/1` over 1000 texts per call: a short title (5 words) or a
/// long paragraph (about 1 KB). The book page "Vectors" quotes it.
#[divan::bench(args = ["short", "long"])]
fn embed_hash_ngram_1000(bencher: divan::Bencher, size: &str) {
    use graphersal::vector::{EmbedContext, Embedder, HashNgram};
    let sentence = "Graph databases store vertices and edges with their properties. ";
    let texts: Vec<String> = (0..1000)
        .map(|i| match size {
            "short" => format!("feeding your cat number {i}"),
            _ => format!("{} {i}", sentence.repeat(16)),
        })
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let embedder = HashNgram::new();
    let ctx = EmbedContext::new();
    bencher.bench(|| embedder.embed(&refs, &ctx).map(|v| v.len()));
}

/// See [`out_out_large_lazy_barrier`]: the same query with the rule disabled.
#[divan::bench(args = [110_000])]
fn out_out_large_no_lazy_barrier(bencher: divan::Bencher, _args: usize) {
    let graph = graphersal::GraphSource::large_110k();
    let lock = graph.read();
    bencher.bench(|| {
        let g = lock
            .traversal()
            .with("optimizer.disabled", vec!["lazy_barrier"]);
        let _ = g.v(None).out(None).out(None).to_list();
    });
}

/// Element property writes, drops, rollbacks and reads on narrow (3 keys) and wide (25 keys)
/// vertices: the baseline for a change of the stored property representation.
mod element_properties {
    use graphersal::changes::RollbackReason;
    use graphersal::prelude::*;
    use graphersal::storage::VertexHandle;

    /// Vertices per graph.
    const VERTICES: usize = 10_000;

    /// `VERTICES` vertices labelled `item`, each with `width` keys `k0..k<width>` (int values).
    fn graph(width: usize) -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        let keys: Vec<String> = (0..width).map(|k| format!("k{k}")).collect();
        for i in 0..VERTICES {
            let props: PropertyMap = keys
                .iter()
                .enumerate()
                .map(|(k, key)| (key.as_str(), ElementProperty::from((i * width + k) as i64)))
                .collect();
            graph
                .insert_vertex(None, vec!["item".into()], props)
                .unwrap();
        }
        graph
    }

    fn handles(graph: &TraversalGraph) -> Vec<VertexHandle> {
        GraphStorage::vertices(graph).collect()
    }

    /// Adds a new key to every vertex (a fresh graph per sample).
    #[divan::bench(args = [3, 25], sample_count = 20)]
    fn write_add_key(bencher: divan::Bencher, width: usize) {
        bencher
            .with_inputs(|| {
                let graph = graph(width);
                let handles = handles(&graph);
                (graph, handles)
            })
            .bench_local_values(|(mut graph, handles)| {
                for &v in &handles {
                    graph
                        .set_vertex_property(v, "added".into(), ElementProperty::from(1))
                        .unwrap();
                }
                graph
            });
    }

    /// Builds the graph through `insert_vertex` (every free-standing map stored by the graph).
    #[divan::bench(args = [1, 3, 25], sample_count = 20)]
    fn insert_build(bencher: divan::Bencher, width: usize) {
        bencher.bench_local(|| graph(width));
    }

    /// A storage-level bulk build: 20k vertices with explicit 36-character ids and six
    /// properties, 60k edges with two, all through `insert_vertex` / `insert_edge`.
    #[divan::bench(sample_count = 10)]
    fn bulk_build(bencher: divan::Bencher) {
        let ids: Vec<String> = (0..20_000u64)
            .map(|i| {
                format!(
                    "{:08x}-0000-4000-8000-{:012x}",
                    i * 2_654_435_761 % (1 << 32),
                    i
                )
            })
            .collect();
        bencher.bench_local(|| {
            let mut graph = TraversalGraph::new();
            let mut handles = Vec::with_capacity(ids.len());
            for (i, id) in ids.iter().enumerate() {
                let props = PropertyMap::from([
                    ("name", ElementProperty::from(id.as_str())),
                    ("age", ElementProperty::from(i as i64)),
                    ("score", ElementProperty::from(i as f64 * 0.5)),
                    ("active", ElementProperty::from(i % 2 == 0)),
                    ("city", ElementProperty::from("berlin")),
                    ("n", ElementProperty::from(i as i64 * 7)),
                ]);
                handles.push(
                    graph
                        .insert_vertex(Some(id.as_str().into()), vec!["person".into()], props)
                        .unwrap(),
                );
            }
            for i in 0..handles.len() * 3 {
                let from = handles[i % handles.len()];
                let to = handles[(i * 7 + 1) % handles.len()];
                let props = PropertyMap::from([
                    ("weight", ElementProperty::from(i as f64)),
                    ("since", ElementProperty::from(i as i64)),
                ]);
                graph
                    .insert_edge(None, Some("knows".into()), from, to, props)
                    .unwrap();
            }
            graph
        });
    }

    /// `vertex_properties(v).get(last key)` on every vertex.
    #[divan::bench(args = [1, 3, 25], sample_count = 20)]
    fn get_key(bencher: divan::Bencher, width: usize) {
        let graph = graph(width);
        let handles = handles(&graph);
        let key = format!("k{}", width - 1);
        bencher.bench_local(|| {
            handles
                .iter()
                .filter_map(|&v| graph.vertex_properties(v).and_then(|p| p.get(&key)))
                .count()
        });
    }

    /// Overwrites the last key of every vertex.
    #[divan::bench(args = [3, 25], sample_count = 20)]
    fn write_overwrite(bencher: divan::Bencher, width: usize) {
        let mut graph = graph(width);
        let handles = handles(&graph);
        let key = format!("k{}", width - 1);
        bencher.bench_local(|| {
            for &v in &handles {
                graph
                    .set_vertex_property(v, key.as_str().into(), ElementProperty::from(7))
                    .unwrap();
            }
        });
    }

    /// Drops the first key of every vertex (a fresh graph per sample).
    #[divan::bench(args = [3, 25], sample_count = 20)]
    fn drop_key(bencher: divan::Bencher, width: usize) {
        bencher
            .with_inputs(|| {
                let graph = graph(width);
                let handles = handles(&graph);
                (graph, handles)
            })
            .bench_local_values(|(mut graph, handles)| {
                for &v in &handles {
                    graph.drop_vertex_property("k0", v).unwrap();
                }
                graph
            });
    }

    /// One unit that overwrites, drops and adds a key on every vertex, rolled back.
    #[divan::bench(args = [3, 25], sample_count = 20)]
    fn rollback_property_changes(bencher: divan::Bencher, width: usize) {
        let mut graph = graph(width);
        let handles = handles(&graph);
        let last = format!("k{}", width - 1);
        bencher.bench_local(|| {
            let mark = graph.begin_unit();
            for &v in &handles {
                graph
                    .set_vertex_property(v, last.as_str().into(), ElementProperty::Null)
                    .unwrap();
                graph.drop_vertex_property("k1", v).unwrap();
                graph
                    .set_vertex_property(v, "added".into(), ElementProperty::from(1))
                    .unwrap();
            }
            graph.rollback_unit(mark, &RollbackReason::failed(&"bench"));
        });
    }

    /// `v().element_map().to_list()`.
    #[divan::bench(args = [3, 25], sample_count = 20)]
    fn element_map(bencher: divan::Bencher, width: usize) {
        let graph = graph(width);
        bencher.bench_local(|| graph.traversal().v(None).element_map(None).to_list());
    }

    /// `v().value_map().to_list()`.
    #[divan::bench(args = [3, 25], sample_count = 20)]
    fn value_map(bencher: divan::Bencher, width: usize) {
        let graph = graph(width);
        bencher.bench_local(|| graph.traversal().v(None).value_map(None).to_list());
    }

    /// `v().values(key).to_list()` on wide vertices: the first, a middle and the last key.
    #[divan::bench(args = ["k0", "k12", "k24"], sample_count = 20)]
    fn values_key_wide(bencher: divan::Bencher, key: &str) {
        let graph = graph(25);
        bencher.bench_local(|| graph.traversal().v(None).values(key).to_list());
    }

    /// `read_snapshot` of a graph whose vertices carry `width` keys. Needs `persist`, which the
    /// bench's required features leave out: run with `--features io,script,display,serde,persist`.
    #[cfg(feature = "persist")]
    #[divan::bench(args = [3, 25], sample_count = 10)]
    fn snapshot_load(bencher: divan::Bencher, width: usize) {
        use graphersal::persist::{self, WriteOptions};
        let mut bytes = Vec::new();
        persist::write_snapshot(&graph(width), &mut bytes, &WriteOptions::new()).unwrap();
        bencher.bench_local(|| TraversalGraph::read_snapshot(bytes.as_slice()).unwrap());
    }
}
