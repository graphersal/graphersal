//! GQL writes (stage 3a, feature `gql`): `INSERT` / Cypher `CREATE`, `FINISH` and queries ending
//! in a write. Differential twins against the Gremlin write steps compare the final graph state
//! (`dump`: every element with its labels and properties, endpoints by their content, sorted),
//! Halloween guards pin the step-at-a-time visibility rule (design W13), `FOR`, `CALL` bodies,
//! `RETURN` after writes, the empty result of a write query, and the read plans stay as they
//! were (a plan dump guard against the plans before stage 3a).
#![cfg(feature = "gql")]

use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;
use graphersal::storage::GraphStorage;

// ----- helpers -----

/// One vertex as text: its labels and its properties sorted by key.
fn vertex_text<G: GraphStorage>(g: &G, v: G::VertexId) -> String {
    let mut props: Vec<String> = g
        .vertex_properties(v)
        .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
        .unwrap_or_default();
    props.sort();
    format!("{:?} {{{}}}", g.vertex_labels(v), props.join(", "))
}

/// The canonical state of `g`: every vertex (labels, properties) and every edge (label,
/// properties, both ends by their content), sorted; independent of element ids.
fn dump<G: GraphStorage>(g: &G) -> Vec<String> {
    let mut lines: Vec<String> = g
        .vertices()
        .map(|v| format!("V {}", vertex_text(g, v)))
        .collect();
    for e in g.edges() {
        let mut props: Vec<String> = g
            .edge_properties(e)
            .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        props.sort();
        let (from, to) = g.edge_endpoints(e).expect("an edge has ends");
        lines.push(format!(
            "E {} {{{}}} from {} to {}",
            g.edge_label(e).unwrap_or_default(),
            props.join(", "),
            vertex_text(g, from),
            vertex_text(g, to)
        ));
    }
    lines.sort();
    lines
}

/// Runs a GQL query on `graph` through the writing source; its rows.
fn write(graph: &mut TraversalGraph, query: &str) -> Vec<ElementProperty> {
    graph
        .traversal_mut()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {}", format_diagnostic(&e)))
}

/// The modern graph after `query`, and its rows.
fn after(query: &str) -> (TraversalGraph, Vec<ElementProperty>) {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let rows = write(&mut graph, query);
    (graph, rows)
}

fn count_label(graph: &TraversalGraph, label: &str) -> usize {
    graph
        .vertices()
        .filter(|&v| graph.vertex_has_label(v, label))
        .count()
}

fn count_edges(graph: &TraversalGraph, label: &str) -> usize {
    graph
        .edges()
        .filter(|&e| graph.edge_label(e) == Some(label))
        .count()
}

/// The column `key` of every row, as text.
fn column(rows: &[ElementProperty], key: &str) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            ElementProperty::Map(map) => map
                .get_str(key)
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
            ElementProperty::Object(map) => map
                .get(key)
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
            other => panic!("not a row map: {other:?}"),
        })
        .collect()
}

/// The optimized plan of `query` on `graph`: every step name with what it records, nested
/// steps indented (the timings left out).
fn plan_dump(graph: &TraversalGraph, query: &str) -> String {
    let metrics = graph
        .traversal()
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"));
    let mut out = String::new();
    walk(metrics.metrics(), 0, &mut out);
    out
}

fn walk(metrics: &[graphersal::profile::Metrics], depth: usize, out: &mut String) {
    for m in metrics {
        out.push_str(&"  ".repeat(depth));
        out.push_str(m.name());
        if let Some(path) = m.path_recording() {
            out.push_str(" [path: ");
            out.push_str(path);
            out.push(']');
        }
        out.push('\n');
        walk(m.nested(), depth + 1, out);
    }
}

/// The step names of the profile of a writing `query` on a copy of the modern graph.
fn write_plan(query: &str, disabled: &[&str]) -> String {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let metrics = graph
        .traversal_mut()
        .with("optimizer.disabled", disabled.to_vec())
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"));
    let mut out = String::new();
    walk(metrics.metrics(), 0, &mut out);
    out
}

// ----- results and endings -----

#[test]
fn a_write_query_returns_no_row() {
    for query in [
        "INSERT (:A)",
        "CREATE (:A {k: 1})",
        "MATCH (p:person) INSERT (p)-[:R]->(:A)",
        "INSERT (:A) FINISH",
        "INSERT (:A);",
    ] {
        let (graph, rows) = after(query);
        assert_eq!(rows, Vec::<ElementProperty>::new(), "{query}");
        assert!(count_label(&graph, "A") >= 1, "{query}");
    }
    // FINISH on a read query: no row, nothing changed.
    let (graph, rows) = after("MATCH (n) FINISH");
    assert!(rows.is_empty());
    assert_eq!(dump(&graph), dump(&TraversalGraph::tinkerpop_modern()));
    // The plan ends in `discard()`.
    let plan = write_plan("INSERT (:A)", &[]);
    assert_eq!(
        plan, "inject(0)\ngql:insert_node([\"A\"])\ndiscard()\n",
        "{plan}"
    );
}

#[test]
fn return_after_insert_reads_the_new_elements() {
    let (graph, rows) =
        after("INSERT (a:A {k: 1})-[r:R {w: 2}]->(b:B) RETURN a.k AS k, r.w AS w, labels(b) AS l");
    assert_eq!(column(&rows, "k"), ["1"]);
    assert_eq!(column(&rows, "w"), ["2"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(count_edges(&graph, "R"), 1);
    // One row per incoming row, in a later MATCH too.
    let (_, rows) = after("MATCH (p:person) INSERT (c:C {n: p.name}) RETURN c.n AS n ORDER BY n");
    assert_eq!(
        column(&rows, "n"),
        ["\"josh\"", "\"marko\"", "\"peter\"", "\"vadas\""]
    );
    let (_, rows) = after("INSERT (a:A)-[:R]->(b:B {k: 7}) MATCH (a)-[:R]->(x) RETURN x.k AS k");
    assert_eq!(column(&rows, "k"), ["7"]);
    // RETURN * lists the named new elements.
    let (_, rows) = after("MATCH (p:person {name: 'marko'}) INSERT (p)-[e:R]->(q:Q) RETURN *");
    let keys: Vec<String> = match &rows[0] {
        ElementProperty::Map(map) => map
            .keys()
            .map(|k| k.as_string().unwrap_or_default().to_string())
            .collect(),
        ElementProperty::Object(map) => map.keys().map(|k| k.to_string()).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(keys, ["p", "e", "q"]);
    // A returned new element is live: Gremlin steps may follow.
    let mut graph = TraversalGraph::tinkerpop_modern();
    let names = graph
        .traversal_mut()
        .gql("MATCH (p:person {name: 'josh'}) INSERT (p)-[:R]->(q:Q) RETURN q")
        .select("q")
        .in_(None)
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names, [ElementProperty::from("josh")]);
}

#[test]
fn a_value_read_before_the_insert_is_the_value_from_before_it() {
    // W10: `k` reads the graph; it is materialized before the INSERT changes the degree.
    let (_, rows) = after(
        "MATCH (p:person {name: 'marko'}) LET k = COUNT { (p)-->() } INSERT (p)-[:R]->(:X) \
         RETURN k, COUNT { (p)-->() } AS now",
    );
    assert_eq!(column(&rows, "k"), ["3"]);
    assert_eq!(column(&rows, "now"), ["4"]);
    let (graph, rows) = after(
        "MATCH (p:person) WITH p, p.age AS a INSERT (p)-[:R]->(:X {age: a}) RETURN a, p.name AS n",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(count_edges(&graph, "R"), 4);
}

#[test]
fn subqueries_in_property_values_run_per_row() {
    let (graph, _) = after(
        "MATCH (p:person) INSERT (:C {who: p.name, n: COUNT { (p)-->() }, out: [(p)-->(x) | x.name]})",
    );
    let lines: Vec<String> = dump(&graph)
        .into_iter()
        .filter(|l| l.starts_with("V [\"C\"]"))
        .collect();
    assert_eq!(lines.len(), 4);
    let marko = lines.iter().find(|l| l.contains("marko")).unwrap();
    assert!(marko.contains("n=3"), "{marko}");
    let vadas = lines.iter().find(|l| l.contains("vadas")).unwrap();
    assert!(vadas.contains("n=0"), "{vadas}");
}

// ----- Halloween guards (design W13) -----

#[test]
fn every_clause_sees_the_writes_before_it_only() {
    // Each clause reads the whole input before the write step runs.
    let (graph, _) = after("MATCH (n) INSERT (:N)");
    assert_eq!(count_label(&graph, "N"), 6);
    assert_eq!(graph.vertices().count(), 12);
    let (graph, _) = after("MATCH (a)-[e]->(b) INSERT (a)-[:X]->(b)");
    assert_eq!(count_edges(&graph, "X"), 6);
    assert_eq!(graph.edges().count(), 12);
    let (graph, _) = after("FOR i IN range(1, 3) MATCH (n) INSERT (:Y)");
    assert_eq!(count_label(&graph, "Y"), 18);
    // A later MATCH sees the writes of every row before it.
    let (_, rows) =
        after("MATCH (n) INSERT (:N) WITH count(*) AS c MATCH (m) RETURN c, count(*) AS all");
    assert_eq!(column(&rows, "c"), ["6"]);
    assert_eq!(column(&rows, "all"), ["12"]);
    // Two clauses: the second one's MATCH sees the first one's nodes.
    let (graph, _) = after("MATCH (n) INSERT (:N) WITH count(*) AS c MATCH (m) INSERT (:M)");
    assert_eq!(count_label(&graph, "M"), 12);
}

// ----- FOR, CALL, OPTIONAL MATCH -----

#[test]
fn for_rows_insert_one_element_each() {
    let (graph, _) = after("FOR x IN [1, 2, 3] INSERT (:F {x: x, sq: x * x})");
    let lines: Vec<String> = dump(&graph)
        .into_iter()
        .filter(|l| l.starts_with("V [\"F\"]"))
        .collect();
    assert_eq!(
        lines,
        [
            "V [\"F\"] {sq=1, x=1}",
            "V [\"F\"] {sq=4, x=2}",
            "V [\"F\"] {sq=9, x=3}",
        ]
    );
    // A FOR item may be an end of an edge.
    let (graph, _) =
        after("MATCH (p:person) WITH collect_list(p) AS ps FOR q IN ps INSERT (q)-[:SELF]->(q)");
    assert_eq!(count_edges(&graph, "SELF"), 4);
    // UNWIND, the Cypher spelling.
    let (graph, _) = after("UNWIND ['a', 'b'] AS n CREATE (:U {n: n})");
    assert_eq!(count_label(&graph, "U"), 2);
}

#[test]
fn a_call_body_writes_per_row_and_the_rows_pass_on() {
    // A unit subquery: every outer row passes on once, the body ran for each.
    let (graph, rows) = after(
        "MATCH (p:person) CALL (p) { INSERT (p)-[:OWNS]->(:Pet {owner: p.name}) } RETURN p.name AS n ORDER BY n",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(count_label(&graph, "Pet"), 4);
    assert_eq!(count_edges(&graph, "OWNS"), 4);
    // A body that creates several rows per outer row still passes the outer row on once.
    let (graph, rows) = after(
        "MATCH (p:person) CALL (p) { MATCH (p)-->(x) INSERT (:Seen {by: p.name}) } RETURN p.name AS n",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(count_label(&graph, "Seen"), 6);
    // OPTIONAL CALL of a unit body: the same.
    let (graph, rows) =
        after("MATCH (p:person) OPTIONAL CALL (p) { INSERT (:O {by: p.name}) } RETURN p.name AS n");
    assert_eq!(rows.len(), 4);
    assert_eq!(count_label(&graph, "O"), 4);
    // A body that writes and returns: its records are the rows.
    let (_, rows) = after(
        "MATCH (p:person) CALL (p) { INSERT (x:X {o: p.name}) RETURN x } RETURN p.name AS n, x.o AS o",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(column(&rows, "n"), column(&rows, "o"));
    // A body ending in FINISH reads only.
    let (graph, rows) =
        after("MATCH (p:person) CALL (p) { MATCH (p)-->(x) FINISH } RETURN p.name AS n");
    assert_eq!(rows.len(), 4);
    assert_eq!(dump(&graph), dump(&TraversalGraph::tinkerpop_modern()));
    // The plan: `optional(__.<body>.discard())`.
    let plan = write_plan(
        "MATCH (p:person) CALL (p) { INSERT (p)-[:OWNS]->(:Pet) } RETURN p.name AS n",
        &[],
    );
    assert!(
        plan.contains("optional(__.gql:insert_node([\"Pet\"]).gql:insert_edge(\"OWNS\", \"p\", \"(#2)\").discard())"),
        "{plan}"
    );
}

#[test]
fn a_null_end_fails_the_query_and_changes_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let error = graph
        .traversal_mut()
        .gql("MATCH (p:person) OPTIONAL MATCH (p)-[:nope]->(q) INSERT (p)-[:R]->(q)")
        .to_list()
        .unwrap_err();
    match error.root_cause() {
        TraverserError::Gql {
            kind, gqlstatus, ..
        } => {
            assert_eq!(*kind, GqlErrorKind::Data);
            assert_eq!(gqlstatus, "22004");
        }
        other => panic!("{other}"),
    }
    assert_eq!(dump(&graph), dump(&TraversalGraph::tinkerpop_modern()));
    // Rows where it is bound insert their edge.
    let (graph, _) = after(
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) FILTER s IS NOT NULL INSERT (s)-[:BY]->(p)",
    );
    assert_eq!(count_edges(&graph, "BY"), 4);
}

#[test]
fn the_read_source_refuses_a_gql_write() {
    let graph = TraversalGraph::tinkerpop_modern();
    assert!(graph.traversal().gql("INSERT (:A)").to_list().is_err());
    assert_eq!(dump(&graph), dump(&TraversalGraph::tinkerpop_modern()));
}

// ----- plans -----

#[test]
fn a_writing_query_has_no_merge_point_of_the_lowering() {
    // Without the optimizer's own rule, the read query keeps the lowering's merge point.
    let off = ["lazy_barrier"];
    let read = TraversalGraph::tinkerpop_modern();
    let plan = read
        .traversal()
        .with("optimizer.disabled", off.to_vec())
        .gql("MATCH (a)-->(b)-->(c) RETURN count(*) AS n")
        .profile()
        .unwrap()
        .to_string();
    assert!(plan.contains("lazy_barrier()"), "{plan}");
    let plan = write_plan(
        "MATCH (a)-->(b)-->(c) INSERT (:T) RETURN count(*) AS n",
        &off,
    );
    assert!(!plan.contains("lazy_barrier()"), "{plan}");
    // Each element is labelled only when read later (D11).
    let plan = write_plan("MATCH (p:person) INSERT (p)-[:R]->(q:Q {n: p.name})", &[]);
    assert_eq!(
        plan,
        "v(labels: [\"person\"])\nas(\"p\") [path: labels(p)]\n\
         gql:insert_node([\"Q\"], \"{n: p.name}\") [path: labels(p)]\n\
         gql:insert_edge(\"R\", \"p\", \"q\")\ndiscard()\n",
        "{plan}"
    );
}

const READ_QUERIES: &[&str] = &[
    "MATCH (p:person)-[:knows]->(f) WHERE f.age > 26 RETURN p.name AS n, f.name AS m",
    "MATCH (a)-->(b)-->(c) RETURN count(*) AS n",
    "MATCH (a)-[e]-(b) RETURN a.name, e.weight, b.name",
    "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name, s.name",
    "MATCH (p:person) WITH p, p.age AS a ORDER BY a LIMIT 2 MATCH (p)-->(x) RETURN p.name, a, x.name",
    "MATCH (p:person)-[:created]->(s) RETURN s.name AS s, count(*) AS n, collect_list(p.name) AS who ORDER BY n DESC",
    "FOR x IN [1, 2, 3] RETURN x * 2 AS y",
    "FOR name IN ['marko', 'josh'] MATCH (p:person {name: name}) RETURN p.age",
    "MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS c } RETURN p.name, c",
    "MATCH (p:person) RETURN p.name AS n UNION MATCH (s:software) RETURN s.name AS n",
    "MATCH (p:person) RETURN p.name AS n NEXT FILTER n <> 'marko' RETURN n",
    "MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name",
    "MATCH (p:person {name: 'marko'})-[e:knows]->{1,2}(f) RETURN f.name, size(e) AS hops",
    "MATCH path = (a:person)-[:created]->(s) RETURN path_length(path) AS l, a.name",
    "MATCH (p:person) LET n = p.name FILTER n STARTS WITH 'm' RETURN n",
    "RETURN 1 + 2 AS x",
    "MATCH (p:person) RETURN p.name, [(p)-->(x) | x.name] AS out, COUNT { (p)-->() } AS c",
    "OPTIONAL MATCH (x:nothing) RETURN x",
];

/// The plans of [`READ_QUERIES`] before stage 3a (main at 5fe9005 plus the 3a parser, binder and
/// steps, which change no read plan).
const READ_PLANS: &str = r##"=== MATCH (p:person)-[:knows]->(f) WHERE f.age > 26 RETURN p.name AS n, f.name AS m
v(labels: ["person"])
as("p") [path: labels(p)]
out("knows") [path: labels(p)]
has("age", P.gt(26)) [path: labels(p)]
gql:project("n", "m")

=== MATCH (a)-->(b)-->(c) RETURN count(*) AS n
v()
out_e()
as("#1") [path: labels(#1)]
in_v() [path: labels(#1)]
lazy_barrier() [path: labels(#1)]
out_e() [path: labels(#1)]
where(P.neq("#1"))
in_v()
gql:aggregate([], ["count(*)"], ["n"])

=== MATCH (a)-[e]-(b) RETURN a.name, e.weight, b.name
v() [path: labels(e)]
as("a") [path: labels(a,e)]
union(__.out_e().as("e").in_v(), __.in_e().gql:not_self_loop().as("e").out_v()) [path: labels(a,e)]
  out_e() [path: labels(a)]
  as("e") [path: labels(a,e)]
  in_v() [path: labels(a,e)]
  in_e() [path: labels(a)]
  gql:not_self_loop() [path: labels(a)]
  as("e") [path: labels(a,e)]
  out_v() [path: labels(a,e)]
gql:project("a.name", "e.weight", "b.name")

=== MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name, s.name
v(labels: ["person"]) [path: labels(s)]
as("p") [path: labels(p,s)]
optional(__.out("created").as("s")) [path: labels(p,s)]
  out("created") [path: labels(p)]
  as("s") [path: labels(p,s)]
gql:project("p.name", "s.name")

=== MATCH (p:person) WITH p, p.age AS a ORDER BY a LIMIT 2 MATCH (p)-->(x) RETURN p.name, a, x.name
v(labels: ["person"])
gql:project("p", "a")
gql:order("a")
limit(2)
as("#w1") [path: labels(#w1)]
gql:bind("p") [path: labels(#w1)]
out() [path: labels(#w1)]
gql:project("p.name", "a", "x.name")

=== MATCH (p:person)-[:created]->(s) RETURN s.name AS s, count(*) AS n, collect_list(p.name) AS who ORDER BY n DESC
v(labels: ["person"])
as("p") [path: labels(p)]
out("created") [path: labels(p)]
gql:aggregate(["s.name"], ["count(*)", "collect_list(p.name)"], ["s", "n", "who"])
gql:order("n DESC")

=== FOR x IN [1, 2, 3] RETURN x * 2 AS y
inject(0)
gql:for("x IN [1, 2, 3]")
gql:project("y")

=== FOR name IN ['marko', 'josh'] MATCH (p:person {name: name}) RETURN p.age
inject(0)
gql:for("name IN ['marko', 'josh']")
as("name") [path: labels(name)]
v() [path: labels(name)]
has_label("person") [path: labels(name)]
gql:filter("name: name")
gql:project("p.age")

=== MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS c } RETURN p.name, c
v(labels: ["person"])
as("p") [path: labels(p)]
flat_map(__.out("created").gql:aggregate([], ["count(s)"], ["c"])) [path: labels(p)]
  out("created")
  gql:aggregate([], ["count(s)"], ["c"])
gql:project("p.name", "c")

=== MATCH (p:person) RETURN p.name AS n UNION MATCH (s:software) RETURN s.name AS n
union(__.v(labels: ["person"]).gql:project("n"), __.v(labels: ["software"]).gql:project("n"))
  v(labels: ["person"])
  gql:project("n")
  v(labels: ["software"])
  gql:project("n")
gql:distinct()

=== MATCH (p:person) RETURN p.name AS n NEXT FILTER n <> 'marko' RETURN n
v(labels: ["person"])
gql:filter("n <> 'marko'")
gql:project("n")

=== MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name
v(labels: ["person"])
filter(__.out("created"))
  out("created")
gql:project("p.name")

=== MATCH (p:person {name: 'marko'})-[e:knows]->{1,2}(f) RETURN f.name, size(e) AS hops
v(labels: ["person"]) [path: labels(e)]
has("name", P.eq("marko")) [path: labels(e)]
repeat(__.out_e("knows").gql:trail("e").as("e").in_v()).times(2).emit() [path: labels(e)]
  out_e("knows") [path: labels(e)]
  gql:trail("e") [path: labels(e)]
  as("e") [path: labels(e)]
  in_v() [path: labels(e)]
gql:project("f.name", "hops")

=== MATCH path = (a:person)-[:created]->(s) RETURN path_length(path) AS l, a.name
v(labels: ["person"])
as("a") [path: labels(a)]
out_e("created") [path: labels(a)]
as("#2") [path: labels(#2,a)]
in_v() [path: labels(#2,a)]
as("s") [path: labels(#2,a,s)]
gql:project("l", "a.name")

=== MATCH (p:person) LET n = p.name FILTER n STARTS WITH 'm' RETURN n
v(labels: ["person"])
has("name", TextP.starting_with("m"))
gql:project("n")

=== RETURN 1 + 2 AS x
inject(0)
gql:project("x")

=== MATCH (p:person) RETURN p.name, [(p)-->(x) | x.name] AS out, COUNT { (p)-->() } AS c
v(labels: ["person"])
gql:project("p.name", "out", "c")
  out()
  gql:value("x.name")
  out()
  count()

=== OPTIONAL MATCH (x:nothing) RETURN x
inject(0) [path: labels(x)]
optional(__.v(labels: ["nothing"]).as("x")) [path: labels(x)]
  v(labels: ["nothing"])
  as("x") [path: labels(x)]
gql:project("x")
"##;

#[test]
fn read_plans_are_unchanged() {
    let graph = TraversalGraph::tinkerpop_modern();
    let mut actual = String::new();
    for query in READ_QUERIES {
        actual.push_str(&format!("=== {query}\n{}\n", plan_dump(&graph, query)));
    }
    assert_eq!(actual.trim_end(), READ_PLANS.trim_end());
}

// ----- cross products feeding a write (fuzz finding of 3a: `MATCH (a), (b) INSERT ()`) -----

#[test]
fn a_cross_product_inserts_one_element_per_row() {
    for (query, rows) in [
        ("MATCH (a), (b) INSERT ()", 36),
        ("MATCH (a) MATCH (b) INSERT ()", 36),
        ("MATCH (a), (b), (c) INSERT ()", 216),
    ] {
        let (graph, result) = after(query);
        assert!(result.is_empty(), "{query}: {result:?}");
        // The later pattern does not see the vertices the write adds (W13): 6 + rows.
        assert_eq!(graph.vertex_count(), 6 + rows, "{query}");
        assert_eq!(graph.edge_count(), 6, "{query}");
    }
    let (graph, _) = after("MATCH (a), (b) INSERT (:X)-[:T]->(:Y)");
    assert_eq!(count_label(&graph, "X"), 36);
    assert_eq!(count_label(&graph, "Y"), 36);
    assert_eq!(count_edges(&graph, "T"), 36);
}

#[test]
fn a_cross_product_over_the_traverser_limit_fails_before_it_is_built() {
    // Run after run the graph grows quadratically: the product is checked against
    // `traversal.max_traversers` before a single traverser of it exists, so a huge one is an
    // error (rolled back), never an allocation of `rows x 80` bytes that aborts the process.
    let mut graph = TraversalGraph::tinkerpop_modern();
    write(&mut graph, "MATCH (a), (b) INSERT ()");
    assert_eq!(graph.vertex_count(), 42);
    let before = dump(&graph);
    let error = graph
        .traversal_mut()
        .with("traversal.max_traversers", 1000)
        .gql("MATCH (a), (b) INSERT ()")
        .to_list()
        .expect_err("1764 rows exceed 1000");
    let text = format_diagnostic(&error);
    assert!(
        text.contains(
            "Resource limit 'traversal.max_traversers' exceeded (limit 1000, reached 1764)"
        ),
        "{text}"
    );
    assert!(
        text.contains("g.with(\"traversal.max_traversers\", 2000).gql("),
        "{text}"
    );
    assert_eq!(dump(&graph), before);
    // The same bound holds for a Gremlin cross product.
    let error = graph
        .traversal()
        .with("traversal.max_traversers", 1000)
        .v(None)
        .v(None)
        .count()
        .to_list()
        .expect_err("1764 traversers exceed 1000");
    assert!(error.to_string().contains("execution failed"), "{error}");
}

// ----- the Rhai binding: twins, the write lock, the create time -----

#[cfg(feature = "script")]
mod script {
    use std::sync::Arc;

    use graphersal::prelude::*;

    use super::dump;
    use crate::script_host::{ScriptGraphTraversalSource, eval_value};

    fn modern() -> Arc<Graph> {
        Arc::new(GraphSource::tinkerpop_modern())
    }

    /// The modern graph after `script`.
    fn state(script: &str) -> Vec<String> {
        let graph = modern();
        let _ = eval_value(graph.clone(), script).unwrap_or_else(|e| panic!("{script}: {e}"));
        let read = graph.read();
        dump(&*read)
    }

    /// The GQL write `query` and its Gremlin twin leave the same graph.
    fn twin(query: &str, gremlin: &str) {
        let gql = state(&format!("g.gql({query:?}).iterate()"));
        let twin = state(&format!("{gremlin}.iterate()"));
        assert_eq!(gql, twin, "{query}\nvs {gremlin}");
        assert_ne!(
            gql,
            dump(&TraversalGraph::tinkerpop_modern()),
            "{query} changes nothing"
        );
    }

    #[test]
    fn cross_product_inserts_leave_the_graph_their_gremlin_twins_leave() {
        twin("MATCH (a), (b) INSERT ()", "g.v().v().add_v()");
        twin("MATCH (a) MATCH (b) INSERT ()", "g.v().v().add_v()");
        twin("MATCH (a), (b), (c) INSERT ()", "g.v().v().v().add_v()");
        twin(
            "MATCH (a), (b) INSERT (:X)-[:T]->(:Y)",
            r#"g.v().v().add_v("X").as("x").add_v("Y").add_e("T").from("x")"#,
        );
        // A barrier on the Gremlin side (bulk merging) leaves the same graph.
        twin("MATCH (a), (b) INSERT ()", "g.v().v().barrier().add_v()");
    }

    #[test]
    fn inserts_leave_the_graph_their_gremlin_twins_leave() {
        twin(
            "INSERT (:P {name: 'x', age: 3})",
            r#"g.add_v("P").property("name", "x").property("age", 3)"#,
        );
        twin(
            "INSERT (:A&B {k: 1}), (:C)",
            r#"g.add_v(["A", "B"]).property("k", 1).add_v("C")"#,
        );
        twin(
            "INSERT (a:A {k: 1})-[:R {w: 2}]->(b:B)",
            r#"g.add_v("A").property("k", 1).as("a").add_v("B").add_e("R").from("a").property("w", 2)"#,
        );
        twin(
            "INSERT (:N {a: 1, b: NULL})",
            r#"g.add_v("N").property("a", 1)"#,
        );
        twin(
            "MATCH (p:person) INSERT (p)-[:OWNS]->(:Pet {owner: p.name})",
            r#"g.v().has_label("person").as("p").add_v("Pet").property("owner", __.select("p").values("name")).add_e("OWNS").from("p")"#,
        );
        twin(
            "MATCH (p:person {name: 'marko'}), (s:software {name: 'lop'}) INSERT (s)<-[:LIKES]-(p)",
            r#"g.v().has("name", "marko").add_e("LIKES").to(__.v().has("name", "lop"))"#,
        );
        twin(
            "MATCH (a)-[e:knows]->(b) CREATE (b)-[:KNOWN_BY {w: e.weight}]->(a)",
            r#"g.e().has_label("knows").as("e").in_v().add_e("KNOWN_BY").to(__.select("e").out_v()).property("w", __.select("e").values("weight"))"#,
        );
        twin(
            "FOR x IN [1, 2, 3] INSERT (:F {x: x})",
            r#"g.inject(1, 2, 3).as("x").add_v("F").property("x", __.select("x"))"#,
        );
        twin(
            "MATCH (p:person) CALL (p) { INSERT (p)-[:OWNS]->(:Pet) } RETURN p",
            r#"g.v().has_label("person").as("p").add_v("Pet").add_e("OWNS").from("p")"#,
        );
    }

    #[test]
    fn a_gql_write_script_takes_the_write_lock() {
        let graph = modern();
        let count = eval_value(
            graph.clone(),
            r#"g.gql("INSERT (:P {name: 'x'})").iterate(); g.v().has_label("P").count().next()"#,
        )
        .unwrap();
        assert_eq!(count.as_int().unwrap(), 1);
        // A traversal returned by the script writes when it runs.
        let graph = modern();
        let mut returned = eval_value(
            graph.clone(),
            r#"g.gql("MATCH (p:person) INSERT (:Q {n: p.name})")"#,
        )
        .unwrap()
        .cast::<ScriptGraphTraversalSource>();
        returned.iterate().unwrap();
        let read = graph.read();
        assert_eq!(
            dump(&*read)
                .iter()
                .filter(|l| l.starts_with("V [\"Q\"]"))
                .count(),
            4
        );
    }

    #[test]
    fn a_gql_write_is_mutating_and_a_read_is_not() {
        for (query, mutating) in [
            ("INSERT (:x)", true),
            ("MATCH (p) INSERT (p)-[:R]->(:x) RETURN p", true),
            ("MATCH (p) CALL (p) { INSERT (:x) } RETURN p", true),
            ("MATCH (p:person) RETURN p", false),
            ("MATCH (p:person) FINISH", false),
            ("MATCH (p) RETRUN p", false),
        ] {
            let traversal = eval_value(modern(), &format!("g.gql({query:?})"))
                .unwrap()
                .cast::<ScriptGraphTraversalSource>();
            assert_eq!(traversal.is_mutating(), mutating, "{query}");
        }
    }

    #[test]
    fn inserted_elements_carry_the_evaluation_time() {
        use crate::script_host::{engine_with_options, graph_scope_with_options};
        use graphersal::temporal::{DateTime, Temporal};
        let schema = GraphSchema::from_json(
            r#"{"mode": "open", "vertices": {"event": {"schema": {"type": "object", "properties": {
                 "at": {"type": "string", "format": "date-time", "defaultFunction": "now"}}}}},
               "edges": {"next": {"schema": {"type": "object", "properties": {
                 "at": {"type": "string", "format": "date-time", "defaultFunction": "now"}}}}}}"#,
        )
        .unwrap();
        let graph = Arc::new(Graph::new(TraversalGraph::new()));
        graphersal::storage::AnyGraph::set_schema(&*graph, schema).unwrap();
        let pinned: DateTime = "2023-08-02T23:30:00-07:00".parse().unwrap();
        let mut engine = engine_with_options(&Default::default(), &[]);
        graphersal::script::pin_now(&mut engine, Some(pinned));
        let mut scope = graph_scope_with_options(graph.clone(), &[]);
        let _ = engine
            .eval_with_scope::<graphersal::script::rhai::Dynamic>(
                &mut scope,
                r#"g.gql("FOR i IN [1, 2] INSERT (:event)-[:next]->(:event)").iterate();
                   g.gql("INSERT (:event)", #{}).iterate()"#,
            )
            .unwrap();
        graphersal::script::pin_now(&mut engine, None);
        let read = graph.read();
        let mut stamps = 0;
        for v in read.vertices() {
            let at = read.vertex_properties(v).and_then(|p| p.get("at")).cloned();
            assert_eq!(
                at,
                Some(ElementProperty::Temporal(Temporal::DateTime(pinned)))
            );
            stamps += 1;
        }
        for e in read.edges() {
            let at = read.edge_properties(e).and_then(|p| p.get("at")).cloned();
            assert_eq!(
                at,
                Some(ElementProperty::Temporal(Temporal::DateTime(pinned)))
            );
            stamps += 1;
        }
        assert_eq!(stamps, 7);
    }
}
