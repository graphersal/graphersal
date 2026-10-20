//! The DSL differential on a foreign storage (`book/src/for_developers/custom_storage.md`): the
//! same scripts run on `Arc<Graph<TraversalGraph>>` and on `Arc<Graph<Forwarding>>`, both built
//! from the modern graph, and their rendered results (and the graph state afterwards) must be
//! equal. `Forwarding` forwards every `GraphStorage` method to a `TraversalGraph` but has its
//! own handle types, so any difference is a bug in the generic front-end path.
//!
//! Covered: every documented DSL example (`script/register/docs.rs`, through the hidden
//! `graphersal::script::doc_examples`) and a curated list of mutating, whole-script
//! (`eval_value_atomic`: commit and rollback), dry-run (`eval_value_dry_run`: the compacted
//! `ChangeSet`), saved-query, schema, catalog, `g.mark` and GraphSON import/export scripts.
#![cfg(all(feature = "script", feature = "io"))]

use std::collections::HashMap;
use std::sync::Arc;

use graphersal::TraversalGraph;
use graphersal::changes::ChangeSet;
use graphersal::storage::{Graph, GraphStorage, IntoAnyGraph};

use crate::forwarding::Forwarding;
use crate::script_host::{
    ScriptLimits, eval_value_atomic, eval_value_dry_run, eval_value_with_limits,
};

/// The reference: the modern graph as a `TraversalGraph`.
fn reference() -> Arc<Graph<TraversalGraph>> {
    Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()))
}

/// The modern graph behind the forwarding storage.
fn forwarded() -> Arc<Graph<Forwarding>> {
    Arc::new(Graph::new(Forwarding::new(
        TraversalGraph::tinkerpop_modern(),
    )))
}

/// `(script).to_string()` on `graph`, or the error text.
fn render(graph: impl IntoAnyGraph, script: &str) -> Result<String, String> {
    eval_value_with_limits(
        graph,
        &format!("({{ {script} }}).to_string()"),
        HashMap::new(),
        &ScriptLimits::default(),
    )
    .map(|value| value.to_string())
    .map_err(|err| err.to_string())
}

#[test]
fn every_documented_example_gives_the_same_result_on_both_storages() {
    let mut differences = Vec::new();
    let mut run = 0;
    for (receiver, name, example, result) in graphersal::script::doc_examples() {
        // `None`: the example needs a file or a host setup (the curated list covers those).
        if result.is_none() {
            continue;
        }
        run += 1;
        let expected = render(reference(), example);
        let actual = render(forwarded(), example);
        if expected != actual {
            differences.push(format!(
                "{receiver}.{name}: `{example}`\n  TraversalGraph: {expected:?}\n  Forwarding:     {actual:?}"
            ));
        }
    }
    assert!(run > 200, "only {run} examples ran");
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// How a curated script runs.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// `eval_value_with_limits`: every traversal is its own unit.
    Plain,
    /// `eval_value_atomic`: the whole script is one unit (commit iff it succeeds).
    Atomic,
    /// `eval_value_dry_run`: the whole script, always rolled back, with its change set.
    DryRun,
}

/// The script that renders the state of a graph after a curated script: data, schema, catalog
/// and statistics.
const STATE: &str = r#"[
    g.V().elementMap().toList(),
    g.E().elementMap().toList(),
    g.get_schema(),
    g.queries().map(|q| [q.name, q.folder, q.description]),
    g.compressions().map(|r| [r.name, r.path, r.compressed_values]),
    g.statistics()
]"#;

/// What a curated script did on one storage: its rendered outcome, the change set of a dry run
/// (compacted), the state afterwards and the commit position.
#[derive(PartialEq, Debug)]
struct Outcome {
    result: Result<String, String>,
    changes: Option<ChangeSet>,
    state: Result<String, String>,
    last_commit_seq: u64,
}

fn run_curated<S>(graph: Arc<Graph<S>>, mode: Mode, script: &str) -> Outcome
where
    S: GraphStorage + Default + Send + Sync + 'static,
{
    let text = format!("({{ {script} }}).to_string()");
    let limits = ScriptLimits::default();
    let (result, changes) = match mode {
        Mode::Plain => (
            eval_value_with_limits(graph.clone(), &text, HashMap::new(), &limits),
            None,
        ),
        Mode::Atomic => (
            eval_value_atomic(graph.clone(), &text, HashMap::new(), &limits),
            None,
        ),
        Mode::DryRun => match eval_value_dry_run(graph.clone(), &text, HashMap::new(), &limits) {
            Ok((value, changes)) => (Ok(value), Some(changes.compact())),
            Err(err) => (Err(err), None),
        },
    };
    let last_commit_seq = graph.read().last_commit_seq();
    Outcome {
        result: result
            .map(|value| value.to_string())
            .map_err(|err| err.to_string()),
        changes,
        state: render(graph, STATE),
        last_commit_seq,
    }
}

/// The curated scripts: `(mode, script)`.
const CURATED: &[(Mode, &str)] = &[
    // Mutations, one unit per traversal.
    (
        Mode::Plain,
        r#"g.addV("person").property("name", "ann").property("age", 41).values("name").next()"#,
    ),
    (
        Mode::Plain,
        r#"g.V("1").addE("likes").to(__.V("3")).property("w", 0.5).label().next()"#,
    ),
    (
        Mode::Plain,
        r#"g.V().hasLabel("software").property("lang", "rust").iterate(); g.V().values("lang").toList()"#,
    ),
    (
        Mode::Plain,
        r#"g.V("1").drop().iterate(); [g.V().count().next(), g.E().count().next()]"#,
    ),
    (
        Mode::Plain,
        r#"g.E().hasLabel("knows").drop().iterate(); g.E().count().next()"#,
    ),
    (
        Mode::Plain,
        r#"g.V("2").properties("age").drop().iterate(); g.V("2").valueMap().next()"#,
    ),
    (
        Mode::Plain,
        r#"g.V("1").property(jpath("address.city"), "Berlin").iterate(); g.V("1").values(jpath("address.city")).next()"#,
    ),
    (
        Mode::Plain,
        r#"g.V("1").add_label("employee").labels().next()"#,
    ),
    (
        Mode::Plain,
        r#"g.mergeV(#{"T.id": "9", "T.label": "person", name: "zed"}).values("name").next()"#,
    ),
    (
        Mode::Plain,
        r#"g.V("1").addE("knows").to(__.V("1")).iterate(); g.V("1").out("knows").id().toList()"#,
    ),
    // Whole-script units: commit and rollback.
    (
        Mode::Atomic,
        r#"g.addV("person").property("name", "bo").next(); g.V("6").drop().iterate(); g.V().count().next()"#,
    ),
    (
        Mode::Atomic,
        r#"g.addV("person").property("name", "bo").next(); throw "boom""#,
    ),
    (
        Mode::Atomic,
        r#"g.V("1").property("age", 30).iterate(); g.V("1").values("age").next()"#,
    ),
    (
        Mode::Atomic,
        r#"g.set_schema(g.infer_schema(), SchemaMode.closed); g.addV("robot").next()"#,
    ),
    // Dry runs: the change set is compared too.
    (
        Mode::DryRun,
        r#"g.addV("person").property("name", "cy").next(); g.V("1").property("age", 30).iterate(); g.V().count().next()"#,
    ),
    (
        Mode::DryRun,
        r#"g.V("1").drop().iterate(); g.E().count().next()"#,
    ),
    (
        Mode::DryRun,
        r#"g.set_schema(g.infer_schema(), SchemaMode.open); g.define_query(#{name: "n", body: "fn n() { 1 }"}); 1"#,
    ),
    // Saved queries.
    (
        Mode::Plain,
        r#"g.define_query(#{name: "older", folder: "people", description: "Older than min.", params: #{min: #{schema: #{type: "integer"}, default: 30}}, body: "fn older(min) { g.v().has(\"age\", P.gt(min)) }"}); [g.query("older").values("name").toList(), g.query("older", #{min: 33}).count().next()]"#,
    ),
    (
        Mode::Plain,
        r#"g.define_queries("reports", "/** Persons. */ fn people() { g.v().has_label(\"person\") }\n/** Software. */ fn software() { g.v().has_label(\"software\") }"); [g.query("people").count().next(), g.query("software").count().next(), g.queries("reports").map(|q| q.name)]"#,
    ),
    (
        Mode::Plain,
        r#"g.define_query(#{name: "n", body: "fn n() { 1 }"}); g.move_query("n", "misc"); [g.drop_query("n"), g.drop_query("n")]"#,
    ),
    (
        Mode::Plain,
        r#"g.define_query(#{name: "w", body: "fn w() { g.addV(\"x\") }"}); g.query("w").next()"#,
    ),
    (Mode::Plain, r#"g.query("missing").next()"#),
    // Schemas in every mode, violations.
    (
        Mode::Plain,
        r#"g.set_schema(g.infer_schema()); g.get_schema()["mode"]"#,
    ),
    (
        Mode::Plain,
        r#"g.set_schema(g.infer_schema(), SchemaMode.open); g.addV("robot").property("x", 1).count().next()"#,
    ),
    (
        Mode::Plain,
        r#"g.set_schema(g.infer_schema(), SchemaMode.closed); g.V("1").property("age", "x").iterate()"#,
    ),
    (
        Mode::Plain,
        r#"g.set_schema(#{mode: "closed", vertices: #{person: #{schema: #{type: "object", properties: #{name: #{type: "integer"}}}}}})"#,
    ),
    (
        Mode::Plain,
        r#"g.validate_schema(#{mode: "open", vertices: #{person: #{schema: #{type: "object", properties: #{age: #{type: "string"}}}}}})"#,
    ),
    (
        Mode::Plain,
        r#"g.set_schema(g.infer_schema(), SchemaMode.open); g.patch_schema(#{vertices: #{robot: #{schema: #{type: "object"}}}})["vertices"].keys()"#,
    ),
    // The catalog: compression rules.
    (
        Mode::Plain,
        r#"g.define_compression(#{name: "names", label: "person", path: "name", min_bytes: 1}); [g.recompress("names"), g.compressions()[0].compressed_values, g.V().hasLabel("person").values("name").toList(), g.drop_compression("names")]"#,
    ),
    // Vector search: nearest(), similarity(), vector_mean(), the schema's metric annotation.
    (
        Mode::Plain,
        r#"g.V("1").property("emb", vector([1.0, 0.0])).iterate(); g.V("2").property("emb", vector([0.6, 0.8], "i8")).iterate(); g.V("4").property("emb", vector([0.0, 1.0])).iterate(); [g.V().nearest("emb", vector([1.0, 0.1]), 2).values("name").toList(), g.V().nearest("emb", [0.0, 1.0], 3, #{metric: "l2"}).id().toList(), g.V().similarity("emb", vector([1.0, 0.0])).toList(), g.V().values("emb").vectorMean().next()]"#,
    ),
    (
        Mode::Plain,
        r#"g.set_schema(#{mode: "none", vertices: #{person: #{schema: #{type: "object", properties: #{emb: #{type: "array", format: "vector", metric: "l2"}}}}}}); g.V("1").property("emb", vector([1.0, 0.0])).iterate(); g.V("2").property("emb", vector([0.0, 1.0])).iterate(); [g.V().hasLabel("person").nearest("emb", [0.9, 0.0], 1).values("name").next(), g.V("1").similarity("emb", [0.0, 0.0]).next()]"#,
    ),
    // has(key, traversal): TinkerPop's filter(values(key).traversal), jpath keys, nested.
    (
        Mode::Plain,
        r#"[g.V().has("name", __.toLower().is(P.within(["marko", "josh"]))).values("name").toList(), g.V().has("age", __.is(P.gt(30))).count().next(), g.V().has("nope", __.identity()).count().next(), g.V().has("person", "name", __.is("vadas")).id().toList(), g.V().where(__.out().has("lang", __.is("java"))).values("name").toList()]"#,
    ),
    (
        Mode::Plain,
        r#"g.V("1").property("meta", #{city: "Berlin"}).iterate(); [g.V().has(jpath("meta.city"), __.to_lower().is("berlin")).id().toList(), g.V().has("age", __.as_string().is("29")).id().toList()]"#,
    ),
    (
        Mode::Atomic,
        r#"g.V().has("name", __.addV("copy")).iterate(); g.V().hasLabel("copy").count().next()"#,
    ),
    // A property index with the transform to_lower: served on the reference, scanned or served
    // on the forwarding storage, the same answers; the default name; a plain index beside it.
    (
        Mode::Plain,
        r#"g.V("1").property("name", "MARKO").iterate(); g.create_index(#{label: "person", key: "name", transform: "to_lower"}); g.create_index(#{label: "person", key: "name"}); [g.indexes().map(|i| [i.name, i.transform, i.entries, i.skipped]), g.V().hasLabel("person").has("name", __.toLower().is("marko")).id().toList(), g.V().hasLabel("person").has("name", __.to_lower().is(P.gte("p"))).values("name").toList(), g.V().hasLabel("person").has("name", __.to_lower().is("MARKO")).count().next()]"#,
    ),
    (
        Mode::Plain,
        r#"g.create_index(#{label: "person", key: "age", transform: "to_lower"}); [g.indexes()[0].skipped, g.indexes()[0].entries]"#,
    ),
    (
        Mode::Plain,
        r#"g.create_index(#{label: "person", key: "age", transform: "to_lower"}); g.V().hasLabel("person").has("age", __.to_lower().is("x")).count().next()"#,
    ),
    // Marks: no journal on either storage.
    (Mode::Plain, r#"g.mark("m")"#),
    (Mode::Atomic, r#"g.addV("x").next(); g.mark("m")"#),
];

/// GQL queries (feature `gql`): node scans, edges in either direction, joins of a repeated
/// variable, trail checks, `ORDER BY` / `OFFSET` / `LIMIT`, parameters, a property index read
/// by the planner (through `GraphStorage::property_indexes`) and errors before and during the run.
#[cfg(feature = "gql")]
const GQL_CURATED: &[&str] = &[
    // Node patterns: labels, label expressions, inline properties, WHERE with NULLs.
    r#"g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS name, p.age AS age").toList()"#,
    r#"g.gql("MATCH (n:!person) RETURN n.name, n.lang, labels(n), element_id(n)").toList()"#,
    r#"g.gql("MATCH (n:person|software {name: 'lop'}) RETURN n").select("n").values("name").toList()"#,
    r#"g.gql("MATCH (n) WHERE n.age IS NULL OR n.age < 30 RETURN n.name, n.age, coalesce(n.age, -1)").toList()"#,
    r#"g.gql("MATCH (n) RETURN *").select("n").id().toList()"#,
    // Edges: each direction, abbreviated, either direction, edge properties and variables.
    r#"g.gql("MATCH (a:person)-[e:knows]->(b) RETURN a.name, e.weight, b.name, type(e)").toList()"#,
    r#"g.gql("MATCH (s:software)<-[:created]-(p) RETURN s.name, p.name").toList()"#,
    r#"g.gql("MATCH (a)-[e]-(b) WHERE a.name = 'josh' RETURN b.name, element_id(e)").toList()"#,
    r#"g.gql("MATCH (a)--(b) RETURN DISTINCT a.name").toList()"#,
    r#"g.gql("MATCH (a)-[e:knows|created WHERE e.weight >= 0.5]->(b) RETURN a.name, b.name").toList()"#,
    // Joins, comma patterns, several MATCH clauses, trail.
    r#"g.gql("MATCH (a)-[:created]->(s)<-[:created]-(b) WHERE a <> b RETURN a.name, b.name, s.name").toList()"#,
    r#"g.gql("MATCH (a)-[:knows]->(b), (b)-[:created]->(c) RETURN a.name, b.name, c.name").toList()"#,
    r#"g.gql("MATCH (a:person) MATCH (a)-[:created]->(s) RETURN a.name, s.name").toList()"#,
    r#"g.gql("MATCH (a)-[e1]-(b)-[e2]-(c) WHERE a.name = 'marko' RETURN b.name, c.name").toList()"#,
    // ORDER BY, OFFSET, LIMIT, CASE, arithmetic, strings.
    r#"g.gql("MATCH (p) RETURN p.name AS n, p.age AS a ORDER BY a DESC, n OFFSET 1 LIMIT 3").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN upper(p.name) || '!' AS n, CASE WHEN p.age > 30 THEN p.age * 2 ELSE p.age % 7 END AS x ORDER BY n").toList()"#,
    // Parameters, and Gremlin steps after gql().
    r#"g.gql("MATCH (p:person) WHERE p.age >= $min RETURN p ORDER BY p.age LIMIT $n", #{min: 29, n: 2}).select("p").out("created").values("name").toList()"#,
    r#"g.gql("MATCH (p {name: $name})-[:knows]->(f) RETURN f.name AS f", #{name: "marko"}).select("f").fold().next()"#,
    // A property index read by the planner: the same rows, and the plan reads the index on both.
    r#"g.create_index(#{label: "person", key: "name"}); let q = "MATCH (p:person {name: $n})-[:created]->(s) RETURN s.name AS s"; [g.gql(q, #{n: "josh"}).toList(), g.gql(q, #{n: "josh"}).profile().to_string().contains("[index: vertex person.name]")]"#,
    // Errors: before the run (syntax, refused write) and during it (division by zero).
    r#"g.gql("MATCH (p:person) RETRUN p").toList()"#,
    r#"g.gql("MATCH (p) SET p.x = 1 RETURN p").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN 100 / (p.age - 29)").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name LIMIT $n", #{n: -1}).toList()"#,
    r#"g.v().gql("MATCH (p) RETURN p").toList()"#,
    // Stage 1b (every entry below runs without an error, so the five error cases above stay
    // the only failing ones). Aggregation: grouping, every aggregate, DISTINCT, NULL skipping.
    r#"g.gql("MATCH (p:person)-[:created]->(s) RETURN s.name AS s, count(*) AS c, sum(p.age) AS total, avg(p.age) AS a, min(p.name) AS lo, max(p.age) AS hi ORDER BY s").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN count(p.age) AS c, collect_list(p.name) AS names").toList()"#,
    r#"g.gql("MATCH (p) RETURN labels(p) AS l, count(DISTINCT p.age) AS c ORDER BY c").toList()"#,
    // OPTIONAL MATCH: unmatched rows read NULL, a WHERE inside the optional part.
    r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS p, s.name AS s ORDER BY p, s").toList()"#,
    r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:knows]->(f) WHERE f.age > 30 RETURN p.name AS p, f.name AS f ORDER BY p").toList()"#,
    // WITH (projecting and materializing), LET, FILTER.
    r#"g.gql("MATCH (p:person) WITH p, p.age * 2 AS dbl WHERE dbl > 60 RETURN p.name AS n, dbl ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person)-[:created]->(s) WITH s, count(*) AS c ORDER BY c DESC LIMIT 1 MATCH (s)<-[:created]-(q) RETURN s.name AS s, c, q.name AS q ORDER BY q").toList()"#,
    r#"g.gql("MATCH (p:person) LET a = p.age + 1 FILTER a > 30 RETURN p.name AS n, a ORDER BY n").toList()"#,
    // Quantified edges: {m,n}, {0,n}, Cypher *m..n with a group variable, +, unbounded.
    r#"g.gql("MATCH (a {name: 'marko'})-[:knows]->{1,2}(b) RETURN b.name AS b ORDER BY b").toList()"#,
    r#"g.gql("MATCH (a)-[:knows|created]->{0,2}(b) WHERE a.name = 'marko' RETURN b.name AS b ORDER BY b").toList()"#,
    r#"g.gql("MATCH (a)-[e:created*1..2]-(b) WHERE a.name = 'lop' RETURN b.name AS b, size(e) AS n ORDER BY b, n").toList()"#,
    r#"g.gql("MATCH (a)-[:knows]->+(b) RETURN b.name AS b ORDER BY b").toList()"#,
    r#"g.gql("MATCH (a)-[:created]-{1,}(b) RETURN count(*) AS c").toList()"#,
    // Path variables: plain edges, a quantified group in either direction.
    r#"g.gql("MATCH p = (a:person)-[:knows]->(b)-[:created]->(c) RETURN p, path_length(p) AS l ORDER BY c.name").toList()"#,
    r#"g.gql("MATCH p = (a {name: 'marko'})-[:created|knows]-{1,2}(b) RETURN length(p) AS l, count(*) AS c ORDER BY l").toList()"#,
    r#"g.gql("MATCH p = (a:person)<-[:knows]-{0,1}(b) RETURN p ORDER BY a.name, length(p)").select("p").toList()"#,
];

/// Stage 1b GQL scripts that fail: data errors of an aggregate, binder refusals, the loop limit
/// of an unbounded quantifier. Both storages fail them the same way.
#[cfg(feature = "gql")]
const GQL_CURATED_1B_ERRORS: &[&str] = &[
    r#"g.gql("MATCH (p) RETURN sum(p.name) AS s").toList()"#,
    r#"g.gql("MATCH (p) RETURN p.name AS n, count(*) AS c ORDER BY p.age").toList()"#,
    r#"g.gql("MATCH (a)-[:knows]->{1,}(b) WITH b RETURN a.name").toList()"#,
    r#"g.with("repeat.max_loops", 1).gql("MATCH (a)-[:created]-{1,}(b) RETURN count(*) AS c").toList()"#,
];

/// Stage 1c GQL functions (feature `gql`): the ones that read the storage beyond the pattern
/// (`keys` / `properties` and their order, `startNode` / `endNode` and `IS SOURCE OF` through
/// `GraphStorage::edge_endpoints`, `nodes` / `relationships` of paths), the pure families on
/// stored values, and data errors, limits and not-yet functions (the last six entries fail).
#[cfg(feature = "gql")]
const GQL_CURATED_1C: &[&str] = &[
    // Elements: keys / properties (order kept), stored endpoints, either direction.
    r#"g.gql("MATCH (n) RETURN element_id(n) AS id, keys(n) AS k, properties(n) AS p ORDER BY id").toList()"#,
    r#"g.gql("MATCH ()-[e]->() RETURN element_id(e) AS id, keys(e) AS k, properties(e) AS p ORDER BY id").toList()"#,
    r#"g.V("1").property("nick", ()).iterate(); g.V("1").property("zeta", 1).iterate(); g.gql("MATCH (n {name: 'marko'}) RETURN keys(n) AS k, properties(n) AS p").toList()"#,
    r#"g.gql("MATCH (a {name: 'josh'})-[e]-(b) RETURN b.name AS b, startNode(e).name AS s, endNode(e).name AS t, a IS SOURCE OF e AS src, b IS DESTINATION OF e AS dst ORDER BY b").toList()"#,
    r#"g.gql("MATCH (a)<-[e:created]-(b) RETURN a.name AS a, element_id(startNode(e)) AS s, element_id(endNode(e)) AS t ORDER BY a, s").toList()"#,
    r#"g.gql("MATCH (a)-[e]->(b) WHERE startNode(e) = a AND endNode(e) = b AND a IS NOT DESTINATION OF e RETURN count(*) AS c").toList()"#,
    // Paths: nodes / relationships, quantified and either direction, zero length, NULL.
    r#"g.gql("MATCH p = (a {name: 'marko'})-[:knows|created]->{1,2}(b) RETURN element_id(b) AS b, length(p) AS l, nodes(p) AS n, relationships(p) AS r ORDER BY b, l").toList()"#,
    r#"g.gql("MATCH p = (a {name: 'marko'})-[:knows|created]->{1,2}(b) RETURN size(relationships(p)) AS l, element_id(head(relationships(p))) AS f, element_id(last(nodes(p))) AS z ORDER BY l, f, z").toList()"#,
    r#"g.gql("MATCH p = (a {name: 'lop'})-[:created]-{0,2}(b) RETURN size(nodes(p)) AS n, size(relationships(p)) AS r, count(*) AS c ORDER BY n, r").toList()"#,
    r#"g.gql("MATCH (a:person) OPTIONAL MATCH p = (a)-[:created]->(s) RETURN a.name AS a, nodes(p) IS NULL AS none ORDER BY a").toList()"#,
    // Conversions, strings, numbers, lists on stored values.
    r#"g.gql("MATCH (n) RETURN n.name AS n, toString(n.age) AS a, CAST(n.age AS FLOAT) AS f, toInteger(n.lang) AS l, toBooleanOrNull(n.name) AS b, toString([n.age, n.name]) AS t ORDER BY n").toList()"#,
    r#"g.gql("MATCH (n) RETURN n.name AS n, left(n.name, 2) AS l, right(n.name, 2) AS r, substring(n.name, 1, 2) AS s, reverse(n.name) AS v, split(n.name, 'o') AS p, replace(n.name, 'o', '0') AS x, btrim(n.name, 'mo') AS b, byte_length(n.name) AS y ORDER BY n").toList()"#,
    r#"g.gql("MATCH (n:person) RETURN n.name AS n, abs(n.age - 30) AS a, round(n.age / 7.0, 2) AS r, sqrt(n.age) AS q, ln(n.age) AS l, power(n.age, 2) AS p, mod(n.age, 7) AS m, sign(n.age - 30) AS s, floor(n.age / 3.0) AS f ORDER BY n").toList()"#,
    r#"g.gql("MATCH (n:person) RETURN n.name AS n, range(1, n.age / 10) AS r, tail(range(1, 3)) AS t, trim(range(1, 4), 1) AS d, isEmpty(n.name) AS e, nullif(n.age, 29) AS z ORDER BY n").toList()"#,
    r#"g.gql("MATCH (n:person) WITH n, round(avg(n.age), 1) AS a RETURN count(*) AS c, max(a) AS m").toList()"#,
    // Failing: a data error per family, the materialization limit, a not-yet function.
    r#"g.gql("MATCH (n) RETURN CAST(n.name AS INTEGER) AS x").toList()"#,
    r#"g.gql("MATCH (n) RETURN substring(n.name, -1) AS x").toList()"#,
    r#"g.gql("MATCH (n:person) RETURN ln(n.age - 29) AS x").toList()"#,
    r#"g.gql("MATCH (n) RETURN startNode(n) AS x").toList()"#,
    r#"g.with("traversal.max_materialized_bytes", 1000).gql("MATCH (n) RETURN range(1, 1000) AS x").toList()"#,
    r#"g.gql("MATCH (n) RETURN elements(n) AS x").toList()"#,
];

/// Stage 2a GQL (feature `gql`): `FOR` / `UNWIND` with ordinals over constant, parameter,
/// per-row and element lists, queries without `MATCH` (a leading `FILTER` included), E3 node
/// items, `OPTIONAL MATCH` after `FOR`, and the correlated lookup `gql:lookup` by id (through
/// `GraphStorage::get_vertex_by_id`) and by property index (through
/// `GraphStorage::vertices_by_property_index`), whose plans name the lookup on both storages;
/// the last two entries fail (a binder error, a data error).
#[cfg(feature = "gql")]
const GQL_CURATED_2A: &[&str] = &[
    r#"g.gql("FOR x IN [1, 2, 2, null] WITH ORDINALITY i RETURN x, i").toList()"#,
    r#"g.gql("FOR x IN [3, 1, [2]] WITH OFFSET i RETURN x, i").toList()"#,
    r#"g.gql("RETURN 1 AS one, 'a' || 'b' AS s, $n + 1 AS m", #{n: 1}).toList()"#,
    r#"g.gql("FILTER $on MATCH (p:person) RETURN count(*) AS c", #{on: true}).toList()"#,
    r#"g.gql("LET k = 3 FOR x IN range(1, k) RETURN collect_list(x * k) AS l").toList()"#,
    r#"g.gql("FOR n IN ['marko', 'josh', 'nobody'] MATCH (p:person {name: n})-[:created]->(s) RETURN n, s.name AS s ORDER BY n, s").toList()"#,
    r#"g.gql("FOR id IN $ids MATCH (n) WHERE element_id(n) = id RETURN id, n.name AS n ORDER BY n", #{ids: ["1", "4", "99", 4, ()]}).toList()"#,
    r#"let q = "FOR id IN $ids MATCH (n) WHERE element_id(n) = id RETURN n.name AS n"; g.gql(q, #{ids: ["1"]}).profile().to_string().contains("[id]")"#,
    r#"g.create_index(#{label: "person", key: "name"}); let q = "FOR n IN $names MATCH (p:person {name: n}) RETURN n, p.age AS a ORDER BY n"; [g.gql(q, #{names: ["marko", "josh", "zed", "marko"]}).toList(), g.gql(q, #{names: ["marko"]}).profile().to_string().contains("gql:lookup(\"(p:person)\", \"name: n\") [index: vertex person")]"#,
    r#"g.create_index(#{label: "person", key: "age"}); g.gql("FOR a IN [29, 29.0, '29', 32, null] OPTIONAL MATCH (p:person {age: a}) RETURN a, p.name AS n ORDER BY n").toList()"#,
    r#"g.create_index(#{label: "software", key: "name"}); g.gql("MATCH (a:person {name: 'josh'}), (s:software {name: 'lop'}) RETURN a.name AS a, s.name AS s").toList()"#,
    r#"g.gql("MATCH p = (a {name: 'marko'})-[:knows]->(b) FOR n IN nodes(p) MATCH (n)-[:created]->(s) RETURN n.name AS n, s.name AS s ORDER BY n, s").toList()"#,
    r#"g.gql("MATCH (p:person)-[:created]->(s) WITH p, collect_list(s) AS ss FOR x IN ss WITH OFFSET i RETURN p.name AS p, x.name AS x, i ORDER BY p, x").toList()"#,
    r#"g.gql("FOR n IN ['marko', 'lop'] OPTIONAL MATCH (p:person {name: n}) RETURN n, p.age AS a ORDER BY n").toList()"#,
    r#"g.gql("MATCH ()-[e]->() FOR w IN [e.weight] FILTER w > 0.5 RETURN element_id(e) AS e, w ORDER BY e").toList()"#,
    r#"g.gql("MATCH (a) FOR a IN [1] RETURN a").toList()"#,
    r#"g.gql("FOR x IN [1, 0] RETURN 1 / x AS y").toList()"#,
];

#[test]
#[cfg(feature = "gql")]
fn curated_gql_2a_scripts_give_the_same_result_on_both_storages() {
    let mut differences = Vec::new();
    let mut failing = 0;
    for script in GQL_CURATED_2A {
        let expected = run_curated(reference(), Mode::Plain, script);
        let actual = run_curated(forwarded(), Mode::Plain, script);
        failing += usize::from(expected.result.is_err());
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  Forwarding:     {actual:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
    // The scripts must exercise FOR and the lookup, not fail on the way.
    assert_eq!(failing, 2, "only the two error cases fail");
    // The plans name the lookup.
    for at in [7, 8] {
        let result = run_curated(reference(), Mode::Plain, GQL_CURATED_2A[at]).result;
        assert!(
            result
                .as_deref()
                .is_ok_and(|text| text.ends_with("true") || text.ends_with("true]")),
            "{at}: {result:?}"
        );
    }
}

/// Stage 2b GQL (feature `gql`): `EXISTS` and pattern predicates on the fast path (`filter()` /
/// `not()` children, the "who has no" plan asserted on both storages) and through the hook in
/// every `gql:*` host (`OR` / `CASE`, `RETURN`, `ORDER BY`, aggregates, `FOR` lists, `LET`),
/// nested and correlated bodies, a `NULL` import, trail inside a body, a lookup by property
/// index inside a body; the last two entries fail (a binder error, a limit).
#[cfg(feature = "gql")]
const GQL_CURATED_2B: &[&str] = &[
    r#"g.gql("MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name AS n").toList()"#,
    r#"g.gql("MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name AS n").profile().to_string().contains("not(__.out(\"created\"))")"#,
    r#"g.gql("MATCH (p) WHERE EXISTS { MATCH (p)-[:created]->(s) WHERE s.lang = 'java' } RETURN p.name AS n ORDER BY n").toList()"#,
    r#"g.gql("MATCH (a:person), (b:person) WHERE (a)-[:knows]->(b) RETURN a.name AS a, b.name AS b ORDER BY b").toList()"#,
    r#"g.gql("MATCH (p:person) WHERE EXISTS ((p)<-[:knows]-()) OR p.age > 33 RETURN p.name AS n ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n, EXISTS { (p)-->() } AS e, CASE WHEN (p)-[:knows]->() THEN 'k' ELSE '-' END AS k ORDER BY EXISTS { (p)-[:created]->() }, n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN count(CASE WHEN (p)-[:created]->() THEN 1 END) AS c, (p)<-[:knows]-() AS known ORDER BY known").toList()"#,
    r#"g.gql("MATCH (p:person) FOR b IN [(p)-[:knows]->(), EXISTS { MATCH (s:software) WHERE s.lang = 'java' }] RETURN p.name AS n, b ORDER BY n, b").toList()"#,
    r#"g.gql("MATCH (p:person) LET k = (p)-[:knows]->() FILTER k OR p.age > 33 RETURN p.name AS n, k ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s:software {lang: 'none'}) RETURN p.name AS n, EXISTS { (s)<-[:created]-() } AS e ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p) WHERE EXISTS { MATCH (p)-->(q) WHERE EXISTS { MATCH (q)-->(r) WHERE NOT (r)-->(p) } } RETURN p.name AS n").toList()"#,
    r#"g.gql("MATCH (a)-[e]->(b) WHERE EXISTS { MATCH (a)-[e]->(b) MATCH (a)-[f]->() WHERE f <> e } RETURN element_id(e) AS e ORDER BY e").toList()"#,
    r#"g.gql("MATCH (a:person) WHERE CASE WHEN EXISTS { MATCH (a)<-[:knows]-() MATCH (a)--() } THEN true ELSE false END RETURN a.name AS n ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) WITH p, count(*) AS c ORDER BY EXISTS { (p)-[:created]->() }, p.name RETURN p.name AS n").toList()"#,
    r#"g.create_index(#{label: "software", key: "name"}); g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s), (t:software {name: s.name}) } RETURN p.name AS n ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) WHERE (p)-[:created]->(s) RETURN p").toList()"#,
    r#"let q = "MATCH (n) WHERE (n)-->()"; for i in 0..65 { q += " OR (n)-->()"; } q += " RETURN n"; g.gql(q).toList()"#,
];

/// Stage 2c GQL (feature `gql`): map projections (`.k`, `.*` in stored order, variable items,
/// entries, a `NULL` target), list comprehensions (filter, map, nesting, over a collected list)
/// and pattern comprehensions in every `gql:*` host (`RETURN`, `WHERE`, `ORDER BY`, aggregates,
/// `FOR`, `WITH`, a `LET`), nested, with a path variable, an element kept live, a `NULL`
/// import, a lookup by property index inside the body, the JSON-shaped answer
/// `p {.name, friends: [..]}` and the `gql:value` child asserted in the profile; the last two
/// entries fail (a binder error, a limit).
#[cfg(feature = "gql")]
const GQL_CURATED_2C: &[&str] = &[
    r#"g.gql("MATCH (p:person) RETURN p {.name, .age} AS m ORDER BY p.name").toList()"#,
    r#"g.gql("MATCH (n) RETURN n {.*} AS m ORDER BY element_id(n)").toList()"#,
    r#"g.gql("MATCH (p:person)-[e:knows]->(f) RETURN p {.name, f, w: e.weight} AS m ORDER BY f.name").toList()"#,
    r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s:software {lang: 'none'}) RETURN p.name AS n, s {.name} AS m ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n, [x IN [p.age, 30, NULL] WHERE x >= 30 | x * 2] AS l ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person)-[:created]->(s) WITH p, collect_list(s.name) AS ns RETURN p.name AS n, [y IN ns WHERE y <> 'lop' | upper(y)] AS l ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person {name: 'marko'}) RETURN p {.name, friends: [(p)-->(f) | f {.name}]} AS answer").toJson()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n, [(p)-[:knows]->(f) | f] AS fs ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) WHERE size([(p)-[:created]->(s) WHERE s.lang = 'java' | s]) > 1 RETURN p.name AS n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n ORDER BY size([(p)<--(x) | x]) DESC, n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN sum(size([(p)-->(x) | x])) AS s, collect_list(size([(p)<--(x) | x])) AS l").toList()"#,
    r#"g.gql("MATCH (p:person {name: 'marko'}) FOR f IN [(p)-[:knows]->(f2) | f2.name] RETURN f ORDER BY f").toList()"#,
    r#"g.gql("MATCH (p:person) WITH p, [(p)-[:created]->(s) | s.name] AS l WHERE size(l) > 0 LET k = [(p)<-[:knows]-(a) | a.name] RETURN p.name AS n, l, k ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n, [(p)-[:knows]->(f) | [(f)-[:created]->(s) | s.name]] AS l, [q = (p)-[:knows]->(f) | [v IN nodes(q) | v.name]] AS ps ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s:software {lang: 'none'}) RETURN p.name AS n, [(s)<-[:created]-(c) | c.name] AS l ORDER BY n").toList()"#,
    r#"g.gql("RETURN size([(a:person)-[:created]->(s:software) WHERE s.name = 'lop' | a.name]) AS c").toList()"#,
    r#"g.create_index(#{label: "software", key: "name"}); g.gql("MATCH (p:person) RETURN p.name AS n, [(p)-[:created]->(s) | size([(t:software {name: s.name})<-[:created]-(c) | c.name])] AS l ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN [(p)-[:knows]->(f) | f.name] AS l").profile().to_string().contains("gql:value(")"#,
    r#"g.gql("MATCH (p:person) RETURN [p IN [1] | p] AS l").toList()"#,
    r#"g.with("traversal.max_materialized_bytes", 64).gql("MATCH (p:person {name: 'marko'}) RETURN [(p)-->(x) | x.name] AS l").toList()"#,
];

/// Stage 2d scripts (feature `gql`): `UNION` / `UNION ALL` with reordered columns, `NEXT` after
/// a linear query and after a `UNION`, `[OPTIONAL] CALL { }` with every scope form (Cypher's
/// importing `WITH` included), nested and with a `UNION` body, top-k per row, `COUNT { }`,
/// `COLLECT { }`, whole-query `EXISTS`, a body served by `gql:lookup` through a property index,
/// the list functions; then the plan of a `CALL` (`flat_map`) and two refused queries.
#[cfg(feature = "gql")]
const GQL_CURATED_2D: &[&str] = &[
    r#"g.gql("MATCH (p:person) RETURN p.name AS n, 'p' AS k UNION MATCH (s:software) RETURN 's' AS k, s.name AS n NEXT RETURN k, n ORDER BY k, n").toList()"#,
    r#"g.gql("MATCH (p:person)-->(x) RETURN x.name AS n UNION ALL MATCH (s:software)<--(y) RETURN y.name AS n NEXT RETURN n, count(*) AS c ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n ORDER BY n DESC LIMIT 2 UNION MATCH (s:software) RETURN s.name AS n ORDER BY n LIMIT 1 NEXT RETURN n ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN p.name AS a, s.name AS b ORDER BY a, b").toList()"#,
    r#"g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS made } RETURN p.name AS n, made ORDER BY n").toList()"#,
    r#"g.gql("MATCH (s:software) CALL (s) { MATCH (s)<-[:created]-(p) RETURN p.name AS who ORDER BY p.age DESC LIMIT 2 } RETURN s.name AS s, who ORDER BY s, who").toList()"#,
    r#"g.gql("MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:created]->(s {lang: 'none'}) RETURN s } RETURN p.name AS n, s ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) CALL { WITH p MATCH (p)-[:knows]->(f) RETURN f.name AS f } RETURN p.name AS n, f ORDER BY n, f").toList()"#,
    r#"g.gql("MATCH (p:person) CALL (*) { CALL (p) { MATCH (p)-->(x) RETURN x UNION MATCH (p)<--(x) RETURN x } RETURN count(x) AS c } RETURN p.name AS n, c ORDER BY n").toList()"#,
    r#"g.gql("FOR i IN [1, 2] CALL { MATCH (s:software) RETURN s.name AS n } RETURN i, n ORDER BY i, n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n, COUNT { MATCH (p)-->(x) RETURN x UNION MATCH (p)<--(x) RETURN x } AS c, COUNT { (p)-[:created]->() } AS m ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n, COLLECT { MATCH (p)-->(x) RETURN x.name ORDER BY x.name } AS l ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WITH s WHERE s.lang = 'java' RETURN s } RETURN p.name AS n ORDER BY n").toList()"#,
    r#"g.create_index(#{label: "person", key: "age"}); g.gql("FOR a IN [29, 32, 40] CALL (a) { MATCH (p:person) WHERE p.age = a RETURN p.name AS n } RETURN a, n ORDER BY a").toList()"#,
    r#"g.create_index(#{label: "person", key: "age"}); g.gql("FOR a IN [29, 32, 40] RETURN a, COUNT { MATCH (p:person) WHERE p.age = a } AS c ORDER BY a").toList()"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS n, all(x IN [p.age, 30] WHERE x > 25) AS a, any(x IN [(p)-->(y) | y.age] WHERE x > 30) AS b, reduce(s = 0, x IN [(p)-->(y) | y] | s + 1) AS r ORDER BY n").toList()"#,
    r#"g.gql("MATCH (p:person) CALL (p) { MATCH (p)-->(f) RETURN f } RETURN p, f").profile().to_string().contains("flat_map(")"#,
    r#"g.gql("MATCH (p:person) RETURN p.name AS a UNION MATCH (s:software) RETURN s.name AS b").toList()"#,
    r#"g.gql("RETURN 1 AS x UNION RETURN 2 AS x UNION ALL RETURN 3 AS x").toList()"#,
];

#[test]
#[cfg(feature = "gql")]
fn curated_gql_2d_scripts_give_the_same_result_on_both_storages() {
    let mut differences = Vec::new();
    let mut failing = 0;
    for script in GQL_CURATED_2D {
        let expected = run_curated(reference(), Mode::Plain, script);
        let actual = run_curated(forwarded(), Mode::Plain, script);
        failing += usize::from(expected.result.is_err());
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  Forwarding:     {actual:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
    // The scripts must exercise stage 2d, not fail on the way: only the two error cases fail.
    assert_eq!(failing, 2, "only the two error cases fail");
    let at = GQL_CURATED_2D.len() - 3;
    let result = run_curated(reference(), Mode::Plain, GQL_CURATED_2D[at]).result;
    assert!(
        result.as_deref().is_ok_and(|text| text.ends_with("true")),
        "{result:?}"
    );
}

#[test]
#[cfg(feature = "gql")]
fn curated_gql_2c_scripts_give_the_same_result_on_both_storages() {
    let mut differences = Vec::new();
    let mut failing = 0;
    for script in GQL_CURATED_2C {
        let expected = run_curated(reference(), Mode::Plain, script);
        let actual = run_curated(forwarded(), Mode::Plain, script);
        failing += usize::from(expected.result.is_err());
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  Forwarding:     {actual:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
    // The scripts must exercise stage 2c, not fail on the way: only the two error cases fail.
    assert_eq!(failing, 2, "only the two error cases fail");
    let at = GQL_CURATED_2C.len() - 3;
    let result = run_curated(reference(), Mode::Plain, GQL_CURATED_2C[at]).result;
    assert!(
        result.as_deref().is_ok_and(|text| text.ends_with("true")),
        "{result:?}"
    );
    // The JSON-shaped answer: marko's name and his three neighbours as maps.
    let result = run_curated(reference(), Mode::Plain, GQL_CURATED_2C[6]).result;
    let text = result.unwrap_or_else(|e| panic!("{e}"));
    for part in [
        "\"answer\"",
        "\"name\": \"marko\"",
        "\"friends\"",
        "\"name\": \"josh\"",
    ] {
        assert!(text.contains(part), "{text}");
    }
}

#[test]
#[cfg(feature = "gql")]
fn curated_gql_2b_scripts_give_the_same_result_on_both_storages() {
    let mut differences = Vec::new();
    let mut failing = 0;
    for script in GQL_CURATED_2B {
        let expected = run_curated(reference(), Mode::Plain, script);
        let actual = run_curated(forwarded(), Mode::Plain, script);
        failing += usize::from(expected.result.is_err());
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  Forwarding:     {actual:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
    // The scripts must exercise EXISTS, not fail on the way: only the two error cases fail.
    assert_eq!(failing, 2, "only the two error cases fail");
    // The "who has no" plan is the not() child on the reference.
    let result = run_curated(reference(), Mode::Plain, GQL_CURATED_2B[1]).result;
    assert!(
        result.as_deref().is_ok_and(|text| text.ends_with("true")),
        "{result:?}"
    );
}

#[test]
#[cfg(feature = "gql")]
fn curated_gql_1c_scripts_give_the_same_result_on_both_storages() {
    let mut differences = Vec::new();
    let mut failing = 0;
    for script in GQL_CURATED_1C {
        let expected = run_curated(reference(), Mode::Plain, script);
        let actual = run_curated(forwarded(), Mode::Plain, script);
        failing += usize::from(expected.result.is_err());
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  Forwarding:     {actual:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
    // The scripts must exercise the functions, not fail on the way: only the error cases fail.
    assert_eq!(failing, 6, "only the six error cases fail");
}

#[test]
#[cfg(feature = "gql")]
fn curated_gql_1b_errors_fail_the_same_way_on_both_storages() {
    for script in GQL_CURATED_1B_ERRORS {
        let expected = run_curated(reference(), Mode::Plain, script);
        let actual = run_curated(forwarded(), Mode::Plain, script);
        assert!(expected.result.is_err(), "`{script}`: {expected:?}");
        assert_eq!(expected, actual, "`{script}`");
    }
}

#[test]
#[cfg(feature = "gql")]
fn curated_gql_scripts_give_the_same_result_on_both_storages() {
    let mut differences = Vec::new();
    for script in GQL_CURATED {
        let expected = run_curated(reference(), Mode::Plain, script);
        let actual = run_curated(forwarded(), Mode::Plain, script);
        if expected != actual {
            differences.push(format!(
                "`{script}`\n  TraversalGraph: {expected:?}\n  Forwarding:     {actual:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
    // The scripts must exercise GQL, not fail on the way: only the error cases fail.
    let failing = GQL_CURATED
        .iter()
        .filter(|script| {
            run_curated(reference(), Mode::Plain, script)
                .result
                .is_err()
        })
        .count();
    assert_eq!(failing, 5, "only the five error cases fail");
    let indexed = run_curated(reference(), Mode::Plain, GQL_CURATED[18]);
    assert!(
        indexed
            .result
            .as_deref()
            .is_ok_and(|text| text.contains("ripple") && text.ends_with("true]")),
        "{indexed:?}"
    );
}

#[test]
fn curated_scripts_give_the_same_result_on_both_storages() {
    let mut differences = Vec::new();
    for (mode, script) in CURATED {
        let expected = run_curated(reference(), *mode, script);
        let actual = run_curated(forwarded(), *mode, script);
        if expected != actual {
            differences.push(format!(
                "{mode:?} `{script}`\n  TraversalGraph: {expected:?}\n  Forwarding:     {actual:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// A file of this test under the system temp directory, removed on drop.
struct TempFile(std::path::PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "graphersal-foreign-storage-{}-{name}",
            std::process::id()
        )))
    }
    fn path(&self) -> String {
        self.0.display().to_string().replace('\\', "/")
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn graphson_export_and_import_give_the_same_result_on_both_storages() {
    // Export: the files are equal.
    let reference_file = TempFile::new("reference.json");
    let forwarded_file = TempFile::new("forwarded.json");
    let export = |path: &str| format!(r#"g.export_graphson("{path}")"#);
    let expected = render(reference(), &export(&reference_file.path()))
        .map(|text| text.replace(&reference_file.path(), "<file>"));
    let actual = render(forwarded(), &export(&forwarded_file.path()))
        .map(|text| text.replace(&forwarded_file.path(), "<file>"));
    assert!(expected.is_ok(), "{expected:?}");
    assert_eq!(expected, actual);
    assert_eq!(
        std::fs::read_to_string(&reference_file.0).unwrap(),
        std::fs::read_to_string(&forwarded_file.0).unwrap()
    );
    // Import (the reference's export) into an emptied graph: same report, same state.
    let import = format!(
        r#"g.V().drop().iterate(); let report = g.import_graphson("{}"); [report, g.V().count().next(), g.E().count().next()]"#,
        reference_file.path()
    );
    let expected = run_curated(reference(), Mode::Plain, &import);
    let actual = run_curated(forwarded(), Mode::Plain, &import);
    assert!(expected.result.is_ok(), "{expected:?}");
    assert_eq!(expected, actual);
    // And as one whole-script unit.
    let expected = run_curated(reference(), Mode::Atomic, &import);
    let actual = run_curated(forwarded(), Mode::Atomic, &import);
    assert_eq!(expected, actual);
}

/// `export_snapshot` on a `Forwarding` graph built by `Graph::new_persistent` writes the
/// snapshot the reference writes (also inside a whole-script unit, which moves the storage into
/// a private graph); built by plain `Graph::new` it is unsupported.
#[cfg(feature = "persist")]
#[test]
fn export_snapshot_writes_the_reference_bytes_on_a_persistent_forwarding_graph() {
    use graphersal::persist::read_snapshot_info;
    // The data sections of a packed snapshot (after the manifest, which records the write
    // time) and the manifest's listing without the write time.
    fn data(path: &std::path::Path) -> (Vec<u8>, graphersal::persist::SnapshotInfo) {
        let bytes = std::fs::read(path).unwrap();
        let len = u64::from_le_bytes(bytes[17..25].try_into().unwrap()) as usize;
        let mut info = read_snapshot_info(bytes.as_slice()).unwrap();
        info.created_at = 0;
        (bytes[25 + len..bytes.len() - 4].to_vec(), info)
    }
    // A fixed lineage id (every segment header carries it), so both graphs write the same bytes.
    fn modern_with_graph_id() -> TraversalGraph {
        let mut graph = TraversalGraph::tinkerpop_modern();
        graphersal::storage::PersistentStorage::set_graph_id(&mut graph, 7);
        graph
    }
    let persistent = || {
        Arc::new(Graph::new_persistent(Forwarding::new(
            modern_with_graph_id(),
        )))
    };
    let export = |path: &str| format!(r#"g.export_snapshot("{path}")"#);
    let reference_file = TempFile::new("reference.gsnap");
    let reference = Arc::new(Graph::new(modern_with_graph_id()));
    let expected = render(reference, &export(&reference_file.path()));
    assert!(expected.is_ok(), "{expected:?}");
    for mode in [Mode::Plain, Mode::Atomic] {
        let forwarded_file = TempFile::new("forwarded.gsnap");
        let actual = run_curated(persistent(), mode, &export(&forwarded_file.path()));
        assert!(actual.result.is_ok(), "{actual:?}");
        assert!(
            data(&forwarded_file.0) == data(&reference_file.0),
            "{mode:?}"
        );
    }

    let forwarded_file = TempFile::new("forwarded.gsnap");
    let err = render(forwarded(), &export(&forwarded_file.path())).unwrap_err();
    assert!(
        err.contains("not supported") || err.contains("does not support"),
        "{err}"
    );
    // The refused export leaves no partial file behind.
    assert!(!forwarded_file.0.exists());
}
