//! `within`/`without` against a side effect and against a long literal list (issue #21).
//!
//! A list or set bucket of 16 members or more, and a literal list of 16 operands or more, is
//! searched through a hash index instead of a scan. The index must never change a result: every
//! differential test here runs the same query once with a short collection (the scan, the
//! behaviour before the index) and once with the collection padded to well above the threshold
//! by strings no probe equals (the index), and expects the same answer. The scaling tests fail
//! with the quadratic scan by a wide margin.
#![cfg(feature = "script")]

use crate::script_host::eval_value;
use graphersal::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Strings no probe of these tests equals: they lift a collection over the index threshold.
fn pads(n: usize) -> String {
    (0..n)
        .map(|i| format!("\"~pad{i}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn run_on(graph: Arc<Graph>, script: &str) -> String {
    let value = eval_value(graph, script).unwrap_or_else(|e| panic!("{script}: {e}"));
    format!("{value:?}")
}

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// 40 `person` vertices `p0..p39`, `age` = i (an `int` for even i, a `float` for odd i), every
/// fifth without an `age`, a chain of `knows` edges `p(i) -> p(i+1)`.
fn people() -> Arc<Graph> {
    let mut graph = TraversalGraph::new();
    let mut previous = None;
    for i in 0..40i64 {
        let mut props = vec![("name", ElementProperty::from(format!("p{i}")))];
        if i % 5 != 4 {
            let age = if i % 2 == 0 {
                ElementProperty::from(i)
            } else {
                ElementProperty::from(i as f64)
            };
            props.push(("age", age));
        }
        let v = graph
            .insert_vertex(
                None,
                vec!["person".into()],
                props.into_iter().collect::<PropertyMap>(),
            )
            .unwrap();
        if let Some(p) = previous {
            graph
                .insert_edge(None, Some("knows".into()), p, v, PropertyMap::default())
                .unwrap();
        }
        previous = Some(v);
    }
    Arc::new(Graph::new(graph))
}

/// Runs `template` with `{pad}` and `{seedpad}` replaced by nothing (a short bucket: the scan) and
/// by padding (a bucket over the threshold: the index): `{pad}` by an
/// `inject([pads]).unfold().aggregate("x").fold().` prefix, `{seedpad}` by `, pads` inside a seed
/// list. Both answers must be equal. Returns the answer.
fn same_with_index(graph: impl Fn() -> Arc<Graph>, template: &str) -> String {
    let scan = template.replace("{pad}", "").replace("{seedpad}", "");
    let indexed = template.replace(
        "{pad}",
        &format!("inject([{}]).unfold().aggregate(\"x\").fold().", pads(40)),
    );
    let indexed = indexed.replace("{seedpad}", &format!(", {}", pads(40)));
    let expected = run_on(graph(), &scan);
    let actual = run_on(graph(), &indexed);
    assert_eq!(actual, expected, "indexed: {indexed}\nscan: {scan}");
    expected
}

/// Both predicates of one template (`{p}` is `within`, then `without`).
fn same_both(graph: impl Fn() -> Arc<Graph> + Copy, template: &str) -> (String, String) {
    (
        same_with_index(graph, &template.replace("{p}", "within")),
        same_with_index(graph, &template.replace("{p}", "without")),
    )
}

const MIXED: &str =
    r#"1, 2.0, -0.0, "a", "1", "", true, false, (), [1, 2], [], #{a: 1}, #{}, 7.5, -3"#;
const PROBES: &str = r#"1, 1.0, 2, 0, 0.0, -0.0, "a", "A", "1", "", true, false, (), [1, 2], [2, 1], [1.0, 2.0], #{a: 1}, #{a: 1.0}, #{}, [], 7.5, -3.0, 4, "~pad1x""#;

#[test]
fn mixed_scalars_and_containers_answer_as_with_a_scan() {
    let (within, without) = same_both(
        modern,
        &format!(
            r#"g.with_side_effect("x", [{MIXED}{{seedpad}}]).inject([{PROBES}]).unfold().where(P.{{p}}("x")).to_list()"#
        ),
    );
    // Spot checks of the equality the scan defines: `1` equals `1.0`, `2` equals `2.0`, `0`
    // equals `-0.0`, `null` equals `null`, a string never equals a number.
    assert!(within.contains("1.0"), "{within}");
    assert!(!within.contains("\"A\""), "{within}");
    assert!(without.contains("\"A\""), "{without}");
    assert!(without.contains('4'), "{without}");
}

#[test]
fn set_buckets_answer_as_with_a_scan() {
    same_both(
        modern,
        &format!(
            r#"g.with_side_effect("x", Set.of()).inject([{MIXED}{{seedpad}}]).unfold().aggregate("x").fold().constant([{PROBES}]).unfold().where(P.{{p}}("x")).to_list()"#
        ),
    );
}

#[test]
fn lazily_read_properties_against_computed_members() {
    // Members read lazily (`values("age")`, ints and floats, a missing age), probes computed.
    same_both(
        people,
        r#"g.v().limit(10).values("age").aggregate("x").fold().{pad}constant([0, 1, 2.0, 3, 8, 9.0, 10, "2", ()]).unfold().where(P.{p}("x")).to_list()"#,
    );
    // Members computed, probes read lazily.
    same_both(
        people,
        r#"g.with_side_effect("x", [0, 1.0, 2, 33, 38.0, "4"]).{pad}v().values("age").where(P.{p}("x")).to_list()"#,
    );
    // Members and probes read lazily, compared through `by()`.
    same_both(
        people,
        r#"g.v().has("age", P.lt(12)).aggregate("x").by("age").fold().{pad}v().where(P.{p}("x")).by("age").values("name").to_list()"#,
    );
    // Ids and labels read lazily.
    same_both(
        modern,
        r#"g.v().limit(3).id().aggregate("x").fold().{pad}v().id().where(P.{p}("x")).to_list()"#,
    );
    same_both(
        modern,
        r#"g.e().limit(2).label().aggregate("x").fold().{pad}e().label().where(P.{p}("x")).to_list()"#,
    );
}

#[test]
fn vertices_edges_and_property_elements_by_identity() {
    same_both(
        modern,
        r#"g.v().has_label("person").limit(2).aggregate("x").fold().{pad}v().where(P.{p}("x")).values("name").to_list()"#,
    );
    same_both(
        modern,
        r#"g.e().limit(3).aggregate("x").fold().{pad}e().where(P.{p}("x")).id().to_list()"#,
    );
    same_both(
        modern,
        r#"g.v().limit(2).properties("name").aggregate("x").fold().{pad}v().properties("name").where(P.{p}("x")).value().to_list()"#,
    );
    // Elements against containers and scalars, and containers against elements.
    same_both(
        modern,
        r#"g.with_side_effect("x", [1, "marko", [1], #{name: "marko"}]).v().limit(2).aggregate("x").fold().{pad}v().where(P.{p}("x")).count().next()"#,
    );
    same_both(
        modern,
        r#"g.v().limit(2).aggregate("x").fold().{pad}v().fold().where(P.{p}("x")).count(Scope.local).next()"#,
    );
    same_both(
        modern,
        r#"g.v().limit(2).value_map().aggregate("x").fold().{pad}v().value_map().where(P.{p}("x")).count().next()"#,
    );
}

#[test]
fn bulked_traversers_keep_their_bulk() {
    // `barrier()` merges the equal traversers of `both()`: the count must expand the bulk.
    let (within, without) = same_both(
        modern,
        r#"g.v().limit(2).aggregate("x").fold().{pad}v().both().barrier().where(P.{p}("x")).count().next()"#,
    );
    let unmerged = run_on(
        modern(),
        r#"g.v().limit(2).aggregate("x").fold().v().both().where(P.within("x")).count().next()"#,
    );
    assert_eq!(within, unmerged);
    let total = run_on(modern(), "g.v().both().count().next()");
    let (w, o, t): (i64, i64, i64) = (
        within.parse().unwrap(),
        without.parse().unwrap(),
        total.parse().unwrap(),
    );
    assert_eq!(w + o, t);
    // A bucket filled from bulked traversers holds every copy, the index sees all of them.
    same_both(
        modern,
        r#"g.v().both().barrier().values("name").aggregate("x").fold().{pad}v().values("name").where(P.{p}("x")).count().next()"#,
    );
}

#[test]
fn a_bucket_that_grows_inside_repeat() {
    // The visited-set BFS: the bucket grows while the loop tests it; the index is extended, never
    // stale. The padded form starts with 40 members, so it is indexed from the first test.
    let visited = same_with_index(
        people,
        r#"g.v().has("name", "p0").aggregate("x").fold().{pad}v().has("name", "p0").repeat(__.both().where(P.without("x")).aggregate("x")).emit().values("name").to_list()"#,
    );
    let expected: Vec<String> = (1..40).map(|i| format!("\"p{i}\"")).collect();
    assert_eq!(visited, format!("[{}]", expected.join(", ")));
    // `store()` grows a bucket one traverser at a time.
    same_with_index(
        modern,
        r#"g.v().limit(1).store("x").fold().{pad}v().limit(1).repeat(__.both().where(P.without("x")).store("x")).emit().values("name").to_list()"#,
    );
}

#[test]
fn a_plan_that_writes_reads_the_current_property_value() {
    // The bucket holds lazy `age` handles; a first test indexes them, the plan then changes an
    // age: the second test must see the new value, as the scan does.
    let answer = same_with_index(
        people,
        r#"g.v().values("age").aggregate("x").fold().{pad}constant([2]).unfold().where(P.within("x")).fold().v().has("name", "p2").property("age", 999).fold().constant([999, 2]).unfold().where(P.within("x")).to_list()"#,
    );
    assert_eq!(answer, "[999]");
}

#[test]
fn long_literal_lists_answer_as_short_ones() {
    let pad = pads(40);
    for (short, long) in [
        (
            r#"g.v().has("age", P.{p}([0, 1, 2.0, 3.0, "4", ()])).values("name").to_list()"#
                .to_string(),
            format!(
                r#"g.v().has("age", P.{{p}}([0, 1, 2.0, 3.0, "4", (), {pad}])).values("name").to_list()"#
            ),
        ),
        (
            r#"g.v().values("age").is(P.{p}([0, 1.0, 2, 33, 38.0])).to_list()"#.to_string(),
            format!(r#"g.v().values("age").is(P.{{p}}([0, 1.0, 2, 33, 38.0, {pad}])).to_list()"#),
        ),
        (
            r#"g.v().properties("age").has_value(P.{p}([1, 2.0, 30])).value().to_list()"#
                .to_string(),
            format!(
                r#"g.v().properties("age").has_value(P.{{p}}([1, 2.0, 30, {pad}])).value().to_list()"#
            ),
        ),
        (
            r#"g.v().values("name").is(P.{p}(["p1", "p10", "P2", 3])).to_list()"#.to_string(),
            format!(r#"g.v().values("name").is(P.{{p}}(["p1", "p10", "P2", 3, {pad}])).to_list()"#),
        ),
    ] {
        for p in ["within", "without"] {
            let short = short.replace("{p}", p);
            let long = long.replace("{p}", p);
            assert_eq!(run_on(people(), &long), run_on(people(), &short), "{long}");
        }
    }
    // A long list of mixed scalar operands (a literal list holds single values only) against
    // computed values of every kind.
    let probes = PROBES;
    let scalars = r#"1, 2.0, -0.0, "a", "1", "", true, false, (), 7.5, -3"#;
    for p in ["within", "without"] {
        let short = format!(r#"g.inject([{probes}]).unfold().is(P.{p}([{scalars}])).to_list()"#);
        let long =
            format!(r#"g.inject([{probes}]).unfold().is(P.{p}([{scalars}, {pad}])).to_list()"#);
        assert_eq!(run_on(modern(), &long), run_on(modern(), &short), "{long}");
    }
}

/// A random graph of `n` vertices (`id` = 0..n) and `m` edges (a fixed LCG, no seed source).
fn random_graph(n: i64, m: usize) -> Arc<Graph> {
    let mut graph = TraversalGraph::new();
    let vertices: Vec<_> = (0..n)
        .map(|i| {
            graph
                .insert_vertex(
                    None,
                    vec!["P".into()],
                    PropertyMap::from([("id", ElementProperty::from(i))]),
                )
                .unwrap()
        })
        .collect();
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as usize % vertices.len()
    };
    for _ in 0..m {
        let (a, b) = (next(), next());
        graph
            .insert_edge(
                None,
                Some("E".into()),
                vertices[a],
                vertices[b],
                PropertyMap::default(),
            )
            .unwrap();
    }
    Arc::new(Graph::new(graph))
}

#[test]
fn the_visited_set_bfs_is_not_quadratic() {
    if crate::ci_support::skip_on_ci("timing") {
        return;
    }
    // 20k vertices, 40k edges: the scan took more than 80 s here (debug build), the index well
    // under a second. The bound is generous for a loaded machine.
    let graph = random_graph(20_000, 40_000);
    let started = Instant::now();
    let reached = run_on(
        graph.clone(),
        r#"g.v().has("P", "id", 1).aggregate("x").repeat(__.both().where(P.without("x")).aggregate("x")).emit().dedup().count().next()"#,
    );
    let elapsed = started.elapsed();
    let dedup = run_on(
        graph,
        r#"g.v().has("P", "id", 1).repeat(__.both().dedup()).emit().count().next()"#,
    );
    // The same vertices are reached, the start excepted (it is in the bucket from the start, and
    // `both()` leads back to it).
    let reachable: i64 = dedup.parse().unwrap();
    let visited: i64 = reached.parse().unwrap();
    assert!(visited > 1000, "{visited}");
    assert_eq!(visited, reachable - 1);
    assert!(
        elapsed < Duration::from_secs(20),
        "visited-set BFS took {elapsed:?}"
    );
}

#[test]
fn a_long_literal_within_is_not_linear_per_traverser() {
    if crate::ci_support::skip_on_ci("timing") {
        return;
    }
    // 20k vertices against 5000 operands: the scan took about 40 s here (debug build).
    let graph = random_graph(20_000, 0);
    let operands = (0..5000)
        .map(|i| (i * 4).to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let started = Instant::now();
    let within = run_on(
        graph.clone(),
        &format!(r#"g.v().has("id", P.within([{operands}])).count().next()"#),
    );
    let without = run_on(
        graph,
        &format!(r#"g.v().has("id", P.without([{operands}])).count().next()"#),
    );
    let elapsed = started.elapsed();
    assert_eq!((within.as_str(), without.as_str()), ("5000", "15000"));
    assert!(elapsed < Duration::from_secs(20), "took {elapsed:?}");
}
