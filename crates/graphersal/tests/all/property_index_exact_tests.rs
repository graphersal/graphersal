//! Proven-exact index accesses drop their re-check (book page "Property Indexes", section "Kept
//! exact"; user decision 2026-11-08): in a plan that does not write, on a storage claiming
//! `StorageCapabilities::exact_property_index_lookups`, a condition an index answers exactly
//! (a point of a string, boolean, uuid or temporal value: a folded `has(key, value)`,
//! `has(key, P.eq/within)`, or `has(key, __.unfold().is(P.eq/within))` by an `each` index) is no
//! longer checked on the candidates, and `.profile()` marks the access `exact`.
//!
//! With the re-check gone the tests are the safety net: a randomized differential test compares
//! every exact shape (and the shapes that keep their re-check) with the scan
//! (`g.with("optimizer.disabled", ["property_index"])`) on random data with arrays (duplicates,
//! nested arrays, objects), scalars of every kind, numbers, nulls, multi-label and unlabeled
//! elements, between random writes and rolled-back writes, comparing the sorted ids AND the
//! counts, and checks the maintained entries against a rebuild after every write. The fast
//! budget runs in every `cargo test`; `PIX_EXACT_SEEDS=<n>` (and `PIX_EXACT_STEPS=<n>`) runs
//! more seeds and steps.
#![cfg(feature = "script")]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;
use graphersal::storage::Graph;

use crate::script_host::{ScriptLimits, eval_value_with_limits};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn run<S: GraphStorage + Default + Send + Sync + 'static>(
    graph: &Arc<Graph<S>>,
    script: &str,
) -> Result<Dynamic, String> {
    eval_value_with_limits(
        graph.clone(),
        script,
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .map_err(|err| err.to_string())
}

fn ok<S: GraphStorage + Default + Send + Sync + 'static>(
    graph: &Arc<Graph<S>>,
    script: &str,
) -> Dynamic {
    run(graph, script).unwrap_or_else(|err| panic!("{script}: {err}"))
}

fn listed(value: Dynamic) -> Vec<String> {
    value
        .into_array()
        .expect("a list")
        .into_iter()
        .map(|item| item.to_string())
        .collect()
}

const SCAN: &str = "g.with(\"optimizer.disabled\", [\"property_index\"])";

/// `query` (after `g`, no terminal) with and without the index: the sorted ids; the indexed
/// result never holds an element twice (unless the query walks edges).
fn both<S: GraphStorage + Default + Send + Sync + 'static>(
    graph: &Arc<Graph<S>>,
    query: &str,
) -> (Vec<String>, Vec<String>) {
    let mut with = listed(ok(graph, &format!("g.{query}.id().to_list()")));
    if !query.contains("out()") {
        let distinct: std::collections::HashSet<&String> = with.iter().collect();
        assert_eq!(distinct.len(), with.len(), "{query}: twice: {with:?}");
    }
    with.sort();
    let mut without = listed(ok(graph, &format!("{SCAN}.{query}.id().to_list()")));
    without.sort();
    (with, without)
}

/// The count of `query` with and without the index.
fn counts<S: GraphStorage + Default + Send + Sync + 'static>(
    graph: &Arc<Graph<S>>,
    query: &str,
) -> (i64, i64) {
    let count = format!("{query}.count().next()");
    (
        ok(graph, &format!("g.{count}")).as_int().unwrap(),
        ok(graph, &format!("{SCAN}.{count}")).as_int().unwrap(),
    )
}

/// The step names of `query`'s profile (`query` after `g`).
fn profile_steps<S: GraphStorage + Default + Send + Sync + 'static>(
    graph: &Arc<Graph<S>>,
    query: &str,
) -> Vec<String> {
    let metrics = ok(graph, &format!("g.{query}.profile()"))
        .try_cast::<graphersal::profile::TraversalMetrics>()
        .expect("metrics");
    metrics
        .metrics()
        .iter()
        .map(|step| step.name().to_string())
        .collect()
}

fn first_step<S: GraphStorage + Default + Send + Sync + 'static>(
    graph: &Arc<Graph<S>>,
    query: &str,
) -> String {
    profile_steps(graph, query)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn check(graph: &Graph) {
    graph.read().check_property_indexes().unwrap();
}

/// A random value of `k`: strings (case variants, look-alikes of other kinds), booleans,
/// numbers, null, arrays with duplicates, nested arrays and objects, an object, a uuid, a
/// datetime (two offsets of one instant), as Rhai source.
fn random_value(rng: &mut Rng) -> String {
    const SCALARS: [&str; 14] = [
        r#""x""#,
        r#""y""#,
        r#""X""#,
        r#""1""#,
        r#""true""#,
        "true",
        "false",
        "1",
        "1.0",
        "2",
        "()",
        "Fake(7).uuid()",
        r#"datetime("2023-08-02T09:30Z")"#,
        r#"datetime("2023-08-02T11:30+02:00")"#,
    ];
    match rng.below(10) {
        0..=3 => SCALARS[rng.below(SCALARS.len())].to_string(),
        4 => r#"#{x: "x"}"#.to_string(),
        _ => {
            let len = rng.below(5);
            let items: Vec<String> = (0..len)
                .map(|_| match rng.below(8) {
                    0 => r#"["x"]"#.to_string(),
                    1 => r#"#{k: "x"}"#.to_string(),
                    _ => SCALARS[rng.below(SCALARS.len())].to_string(),
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
    }
}

/// Vertices labelled `a`, `b`, both or none, with a random `k` (sometimes missing) and a `t`
/// of two values; edges labelled `r` or `s` with a random `k`.
fn random_graph(rng: &mut Rng, vertices: usize) -> Arc<Graph> {
    let graph = Arc::new(GraphSource::empty());
    let label_sets = [r#"["a"]"#, r#"["b"]"#, r#"["a", "b"]"#, "[]"];
    let mut script = String::new();
    for i in 0..vertices {
        let labels = label_sets[rng.below(label_sets.len())];
        script.push_str(&format!(
            "g.add_v({labels}).property(\"name\", \"v{i}\").property(\"t\", \"t{}\")",
            rng.below(2)
        ));
        if rng.below(6) != 0 {
            script.push_str(&format!(".property(\"k\", {})", random_value(rng)));
        }
        script.push_str(".iterate();\n");
    }
    let _ = ok(&graph, &script);
    let mut script = String::new();
    for _ in 0..vertices {
        let label = if rng.below(2) == 0 { "r" } else { "s" };
        script.push_str(&format!(
            "g.v().has(\"name\", \"v{}\").add_e(\"{label}\").to(__.v().has(\"name\", \"v{}\")).property(\"k\", {}).iterate();\n",
            rng.below(vertices),
            rng.below(vertices),
            random_value(rng)
        ));
    }
    let _ = ok(&graph, &script);
    graph
}

/// The indexes of the random graph: label-less plain and `each` on `k` (vertices and edges),
/// per-label plain and `each` on `a.k`, per-label plain on edge `r.k`.
fn create_indexes<S: GraphStorage + Default + Send + Sync + 'static>(graph: &Arc<Graph<S>>) {
    for index in [
        r#"#{key: "k"}"#,
        r#"#{key: "k", transform: "each"}"#,
        r#"#{label: "a", key: "k"}"#,
        r#"#{label: "a", key: "k", transform: "each"}"#,
        r#"#{element: "edge", key: "k"}"#,
        r#"#{element: "edge", key: "k", transform: "each"}"#,
        r#"#{element: "edge", label: "r", key: "k"}"#,
    ] {
        let _ = ok(graph, &format!("g.create_index({index})"));
    }
}

/// The point values the queries ask for, as Rhai source.
const POINTS: [&str; 8] = [
    r#""x""#,
    r#""y""#,
    r#""1""#,
    "true",
    "false",
    "Fake(7).uuid()",
    r#"datetime("2023-08-02T09:30Z")"#,
    r#"datetime("2023-08-02T11:30+02:00")"#,
];

/// The queries (after `g`) and whether the plan answers the index's condition exactly; `X`
/// and `Y` are replaced with two points.
const QUERIES: [(&str, bool); 27] = [
    // Exact: points of exact values.
    (r#"v().has("k", X)"#, true),
    (r#"v().has("k", P.eq(X))"#, true),
    (r#"v().has("k", P.within([X, Y, X]))"#, true),
    (r#"v().has("k", __.unfold().is(X))"#, true),
    (r#"v().has("k", __.unfold().is(P.within([X, Y])))"#, true),
    (r#"v().has_label("a").has("k", X)"#, true),
    (r#"v().has_label("a").has("k", __.unfold().is(X))"#, true),
    // A label-less index under labelled sources: the label is still checked.
    (r#"v().has_label("b").has("k", X)"#, true),
    (
        r#"v().has_label("a", "b").has("k", __.unfold().is(X))"#,
        true,
    ),
    // An extra condition the index does not answer is still checked.
    (r#"v().has("k", X).has("t", "t1")"#, true),
    (r#"v().has("t", "t0").has("k", __.unfold().is(X))"#, true),
    (r#"v().has("k", __.unfold().is(X)).has_label("b")"#, true),
    (r#"e().has("k", X)"#, true),
    (r#"e().has("k", __.unfold().is(X))"#, true),
    (r#"e().has_label("r").has("k", X)"#, true),
    (
        r#"e().has_label("s").has("k", __.unfold().is(P.within([X, Y])))"#,
        true,
    ),
    // A `has` after a step is not served; a child level starting with `v()` is.
    (r#"v().has_label("a").out().has("k", X)"#, false),
    (
        r#"v().has_label("b").limit(1).flat_map(__.v().has("k", __.unfold().is(X)))"#,
        true,
    ),
    // Not exact: numbers, null, ranges, a superset, a value inside a P that is not a point.
    (r#"v().has("k", 1)"#, false),
    (r#"v().has("k", __.unfold().is(1))"#, false),
    (r#"v().has("k", __.unfold().is(P.eq(())))"#, false),
    (r#"v().has("k", P.gte("x"))"#, false),
    (r#"v().has("k", __.unfold().is(P.lt("y")))"#, false),
    (r#"v().has("k", P.within([X, 1]))"#, false),
    (r#"v().has("k", __.unfold().is(P.within([X, 2])))"#, false),
    (r#"v().has("t", "t0").has("k", P.neq(X))"#, false),
    (r#"e().has("k", P.between("a", "y"))"#, false),
];

/// Writes between the queries; the last ones fail and roll back.
const WRITES: [&str; 12] = [
    r#"g.v().has("k", __.unfold().is("x")).limit(2).property("k", VALUE).iterate()"#,
    r#"g.v().has("k", "y").limit(1).properties("k").drop().iterate()"#,
    r#"g.add_v("a").property("k", VALUE).iterate()"#,
    r#"g.add_v(["a", "b"]).property("k", VALUE).property("t", "t1").iterate()"#,
    r#"g.v().has("k", __.unfold().is(true)).limit(1).drop().iterate()"#,
    r#"g.v().has_label("b").limit(1).add_label("a").iterate()"#,
    r#"g.v().has_label("a").limit(1).drop_label("a").iterate()"#,
    r#"g.e().has("k", __.unfold().is("x")).limit(1).property("k", VALUE).iterate()"#,
    r#"g.e().limit(1).drop().iterate()"#,
    r#"g.v().has("k", "x").limit(1).property("k", VALUE).v().fail("roll back").iterate()"#,
    r#"g.add_v("a").property("k", VALUE).v().fail("roll back").iterate()"#,
    r#"g.e().limit(3).property("k", VALUE).v().fail("roll back").iterate()"#,
];

fn env_number(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// One seed: a random graph, its indexes, then `steps` rounds of a random write followed by
/// every query (with two random points) compared with the scan, ids and counts.
fn differential(seed: u64, vertices: usize, steps: usize) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let graph = random_graph(&mut rng, vertices);
    create_indexes(&graph);
    check(&graph);
    let mut exercised = 0usize;
    for step in 0..=steps {
        if step > 0 {
            let write = WRITES[rng.below(WRITES.len())].replace("VALUE", &random_value(&mut rng));
            let _ = run(&graph, &write);
            check(&graph);
        }
        for (template, exact) in QUERIES {
            let x = POINTS[rng.below(POINTS.len())];
            let y = POINTS[rng.below(POINTS.len())];
            let query = template.replace('X', x).replace('Y', y);
            let plan = profile_steps(&graph, &query).join(" | ");
            assert_eq!(
                plan.contains(", exact]"),
                exact,
                "seed {seed} step {step}: {query}: {plan}"
            );
            let (with, without) = both(&graph, &query);
            assert_eq!(with, without, "seed {seed} step {step}: {query}");
            let (with, without) = counts(&graph, &query);
            assert_eq!(with, without, "seed {seed} step {step}: {query}.count()");
            exercised += usize::from(with > 0);
        }
    }
    assert!(
        exercised > steps,
        "seed {seed}: the queries found almost nothing"
    );
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn every_exact_shape_equals_the_scan_on_random_data_between_writes_and_rollbacks() {
    let seeds = env_number("PIX_EXACT_SEEDS", 6) as u64;
    let steps = env_number("PIX_EXACT_STEPS", 10);
    for seed in 1..=seeds {
        differential(seed, 120, steps);
    }
}

/// A heavier run of the same test (more data, more seeds); `PIX_EXACT_SEEDS` and
/// `PIX_EXACT_STEPS` scale it further.
#[test]
#[ignore = "heavy: cargo test --release ... -- --ignored"]
fn every_exact_shape_equals_the_scan_heavy() {
    let seeds = env_number("PIX_EXACT_SEEDS", 40) as u64;
    let steps = env_number("PIX_EXACT_STEPS", 30);
    for seed in 100..100 + seeds {
        differential(seed, 400, steps);
    }
}

fn owners() -> Arc<Graph> {
    let graph = Arc::new(GraphSource::empty());
    let _ = ok(
        &graph,
        r#"
        g.add_v("Method").property("_owners", ["o1", "o2", "o1"]).property("name", "m1").iterate();
        g.add_v("Method").property("_owners", "o1").property("name", "m2").iterate();
        g.add_v("Class").property("_owners", ["o2"]).property("name", "c1").iterate();
        g.add_v().property("_owners", [["o1"], #{o: "o1"}, 1]).property("name", "n1").iterate();
        g.v().has("name", "m1").add_e("CALLS").to(__.v().has("name", "m2")).property("_owner", "o1").iterate();
        g.create_index(#{key: "_owners", transform: "each"});
        g.create_index(#{key: "name"});
        g.create_index(#{element: "edge", key: "_owner"});
        "#,
    );
    graph
}

#[test]
fn the_owner_query_drops_its_recheck_and_counts_from_the_postings() {
    let graph = owners();
    let steps = profile_steps(
        &graph,
        r#"v().has("_owners", __.unfold().is("o1")).count()"#,
    );
    assert_eq!(
        steps,
        [
            r#"v().has("_owners", __.unfold().is(P.eq("o1"))).count() [index: vertex _owners each, any label, exact] [count from index]"#
        ]
    );
    assert_eq!(
        counts(&graph, r#"v().has("_owners", __.unfold().is("o1"))"#),
        (2, 2)
    );
    // Every query form gains: ids, a traversal from the found elements.
    let steps = profile_steps(&graph, r#"v().has("_owners", __.unfold().is("o1")).out()"#);
    assert_eq!(
        steps,
        [
            r#"v().has("_owners", __.unfold().is(P.eq("o1"))) [index: vertex _owners each, any label, exact]"#,
            "out()"
        ]
    );
    let (with, without) = both(&graph, r#"v().has("_owners", __.unfold().is("o1")).out()"#);
    assert_eq!(with, without);
    // A label-less plain index on a name, an edge index.
    assert_eq!(
        first_step(&graph, r#"v().has("name", "m1").id()"#),
        r#"v().has("name", "m1") [index: vertex name, any label, exact]"#
    );
    assert_eq!(
        first_step(&graph, r#"e().has("_owner", "o1").count()"#),
        r#"e().has("_owner", "o1").count() [index: edge _owner, any label, exact] [count from index]"#
    );
    // A labelled source over the label-less index: exact, but the label is checked (no count
    // from the postings).
    let steps = profile_steps(
        &graph,
        r#"v().has_label("Method").has("_owners", __.unfold().is("o2")).count()"#,
    );
    assert_eq!(
        steps,
        [
            r#"v(labels: ["Method"]).has("_owners", __.unfold().is(P.eq("o2"))) [index: vertex _owners each, any label, exact]"#,
            "count()"
        ]
    );
    assert_eq!(
        counts(
            &graph,
            r#"v().has_label("Method").has("_owners", __.unfold().is("o2"))"#
        ),
        (1, 1)
    );
    // `group_count()` over an exact access counts the found elements, not every label.
    assert_eq!(
        ok(
            &graph,
            r#"g.v().has("_owners", __.unfold().is("o2")).group_count().by(T.label).next()"#
        )
        .to_string(),
        ok(
            &graph,
            &format!(
                r#"{SCAN}.v().has("_owners", __.unfold().is("o2")).group_count().by(T.label).next()"#
            )
        )
        .to_string()
    );
}

#[test]
fn the_recheck_stays_where_the_index_does_not_answer_exactly() {
    let graph = owners();
    for query in [
        // A superset: arrays holding the value are candidates of a whole-value condition.
        r#"v().has("_owners", "o1")"#,
        // A number, null, a range.
        r#"v().has("_owners", __.unfold().is(1))"#,
        r#"v().has("_owners", __.unfold().is(P.eq(())))"#,
        r#"v().has("_owners", __.unfold().is(P.gt("o1")))"#,
        r#"v().has("name", P.lt("n"))"#,
    ] {
        let plan = first_step(&graph, query);
        assert!(plan.contains("[index"), "{query}: {plan}");
        assert!(!plan.contains("exact"), "{query}: {plan}");
        let (with, without) = both(&graph, query);
        assert_eq!(with, without, "{query}");
    }
    // A superset keeps the `has` step and its result.
    assert_eq!(
        both(&graph, r#"v().has("_owners", "o1")"#).0.len(),
        1,
        "only the scalar"
    );
    // A `to_lower` transform can fail: it keeps its re-check (user decision).
    let _ = ok(
        &graph,
        r#"g.create_index(#{key: "name", transform: "to_lower"})"#,
    );
    let plan = first_step(&graph, r#"v().has("name", __.to_lower().is("m1"))"#);
    assert!(plan.contains("to_lower, any label]"), "{plan}");
    assert_eq!(
        profile_steps(&graph, r#"v().has("name", __.to_lower().is("m1"))"#).len(),
        2,
        "the has() step stays"
    );
}

#[test]
fn a_plan_that_writes_keeps_every_recheck() {
    let graph = owners();
    let writing = [
        r#"v().has("_owners", __.unfold().is("o1")).property("seen", true)"#,
        r#"v().has("name", "m1").property("name", "m9")"#,
        // A write in a child level counts too.
        r#"v().has("name", "m2").union([__.identity(), __.add_v("x")])"#,
        r#"v().has("_owners", __.unfold().is("o2")).side_effect(__.property("t", 1))"#,
    ];
    for query in writing {
        let steps = profile_steps(&graph, query);
        assert!(steps[0].contains("[index"), "{query}: {steps:?}");
        assert!(
            !steps.iter().any(|step| step.contains("exact")),
            "{query}: {steps:?}"
        );
    }
    graph.read().check_property_indexes().unwrap();
    // The same reading query after the writes: exact again, and right.
    let query = r#"v().has("name", "m9")"#;
    assert!(first_step(&graph, query).ends_with("any label, exact]"));
    assert_eq!(both(&graph, query).0.len(), 1);
}

#[test]
fn gql_root_scans_share_the_exact_access() {
    let graph = owners();
    let text = ok(
        &graph,
        r#"g.gql("MATCH (n {name: 'm1'}) RETURN count(n) AS c").profile().to_string()"#,
    )
    .to_string();
    assert!(
        text.contains("[index: vertex name, any label, exact] [count from index]"),
        "{text}"
    );
    assert_eq!(
        ok(
            &graph,
            r#"g.gql("MATCH (n {name: 'm1'}) RETURN count(n) AS c").to_list()"#
        )
        .to_string(),
        r#"[#{"c": 1}]"#
    );
}

/// The forwarding storage forwards the capability: the same exact plans and results.
#[test]
fn the_forwarding_storage_claims_and_serves_exact_lookups() {
    use crate::forwarding::Forwarding;
    let mut rng = Rng(77);
    let reference = random_graph(&mut rng, 80);
    let inner = std::mem::take(&mut *reference.write());
    let forwarded = Arc::new(Graph::new(Forwarding::new(inner)));
    assert!(
        forwarded
            .read()
            .capabilities()
            .unwrap()
            .exact_property_index_lookups
    );
    create_indexes(&forwarded);
    for (template, exact) in QUERIES {
        let query = template.replace('X', r#""x""#).replace('Y', "true");
        let plan = profile_steps(&forwarded, &query).join(" | ");
        assert_eq!(plan.contains(", exact]"), exact, "{query}: {plan}");
        let (with, without) = both(&forwarded, &query);
        assert_eq!(with, without, "{query}");
        let (with, without) = counts(&forwarded, &query);
        assert_eq!(with, without, "{query}");
    }
}
