//! GQL writes (stage 3a, feature `gql`): `INSERT` / Cypher `CREATE`, `FINISH` and queries ending
//! in a write. Differential twins against the Gremlin write steps compare the final graph state
//! (`dump`: every element with its labels and properties, endpoints by their content, sorted),
//! Halloween guards pin the step-at-a-time visibility rule (design W13), `FOR`, `CALL` bodies,
//! `RETURN` after writes, the empty result of a write query, and the read plans stay as they
//! were (a plan dump guard against the plans before stage 3a). Stage 3c: `SET` / `REMOVE`
//! twins against `property()`, `remove_property()`, `add_label()`, `drop_label()` and
//! `property(map)`, values read across a write (W10), one evaluation per row (W15),
//! read-after-write (an index included), the schema, `CALL` bodies and saved queries. Stage 3d:
//! `DELETE` / `DETACH DELETE` twins against `drop()`, the NODETACH check per clause (G1001,
//! nothing removed), paths, lists, `NULL`, twice, `count(n)` after a DELETE, reads and returns
//! of a deleted element (W27 / W28), `CALL` bodies and saved queries. Stage 3e: `MERGE` twins
//! against `merge_v()` / `merge_e()`, the `coalesce()` idioms and the pure-GQL form
//! `CALL { FILTER NOT EXISTS { .. } INSERT .. }`, bound ends (`WITH` columns, NULL), ON items in
//! written order, NULL property values (E1), several MERGEs in one query, an index-served match,
//! `CALL` bodies, the create time and saved queries.
#![cfg(feature = "gql")]

use graphersal::error::GqlErrorKind;
use graphersal::prelude::*;
use graphersal::storage::GraphStorage;

// ----- helpers -----

/// One vertex as text: its labels and its properties sorted by key.
fn vertex_text<G: GraphStorage>(g: &G, v: G::VertexId) -> String {
    let mut props: Vec<String> = g
        .vertex_properties(v)
        .unwrap()
        .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
        .unwrap_or_default();
    props.sort();
    format!("{:?} {{{}}}", g.vertex_labels(v).unwrap(), props.join(", "))
}

/// The canonical state of `g`: every vertex (labels, properties) and every edge (label,
/// properties, both ends by their content), sorted; independent of element ids.
fn dump<G: GraphStorage>(g: &G) -> Vec<String> {
    let mut lines: Vec<String> = g
        .vertices()
        .unwrap()
        .map(|v| format!("V {}", vertex_text(g, v)))
        .collect();
    for e in g.edges().unwrap() {
        let mut props: Vec<String> = g
            .edge_properties(e)
            .unwrap()
            .map(|map| map.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        props.sort();
        let (from, to) = g.edge_endpoints(e).unwrap().expect("an edge has ends");
        lines.push(format!(
            "E {} {{{}}} from {} to {}",
            g.edge_label(e).unwrap().unwrap_or_default(),
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
        .unwrap()
        .filter(|&v| graph.vertex_has_label(v, label).unwrap())
        .count()
}

fn count_edges(graph: &TraversalGraph, label: &str) -> usize {
    graph
        .edges()
        .unwrap()
        .filter(|&e| graph.edge_label(e).unwrap() == Some(label))
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
    assert_eq!(keys, ["e", "p", "q"]);
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
    assert_eq!(graph.vertices().unwrap().count(), 12);
    let (graph, _) = after("MATCH (a)-[e]->(b) INSERT (a)-[:X]->(b)");
    assert_eq!(count_edges(&graph, "X"), 6);
    assert_eq!(graph.edges().unwrap().count(), 12);
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
        for v in read.vertices().unwrap() {
            let at = read
                .vertex_properties(v)
                .unwrap()
                .and_then(|p| p.get("at"))
                .cloned();
            assert_eq!(
                at,
                Some(ElementProperty::Temporal(Temporal::DateTime(pinned)))
            );
            stamps += 1;
        }
        for e in read.edges().unwrap() {
            let at = read
                .edge_properties(e)
                .unwrap()
                .and_then(|p| p.get("at"))
                .cloned();
            assert_eq!(
                at,
                Some(ElementProperty::Temporal(Temporal::DateTime(pinned)))
            );
            stamps += 1;
        }
        assert_eq!(stamps, 7);
    }

    /// Stage 3c: `SET` / `REMOVE` leave the graph their Gremlin twins leave (a NULL value
    /// compares against `remove_property()`: GQL removes the key, `property(k, null)` would
    /// store a null).
    #[test]
    fn set_and_remove_leave_the_graph_their_gremlin_twins_leave() {
        for (query, gremlin) in [
            (
                "MATCH (p:person) SET p.k = 1",
                r#"g.v().has_label("person").property("k", 1)"#,
            ),
            (
                "MATCH (p {name: 'marko'}) SET p.k = p.age, p.j = p.k",
                r#"g.v().has("name", "marko").property("k", 29).property("j", 29)"#,
            ),
            (
                "MATCH (p {name: 'marko'}) SET p.age = NULL",
                r#"g.v().has("name", "marko").remove_property("age")"#,
            ),
            (
                "MATCH (p:person) REMOVE p.age",
                r#"g.v().has_label("person").remove_property("age")"#,
            ),
            (
                "MATCH (p:person) SET p:Old:Tall",
                r#"g.v().has_label("person").add_label("Old").add_label("Tall")"#,
            ),
            (
                "MATCH (p:person) SET p IS Old & Tall, p:Old",
                r#"g.v().has_label("person").add_label("Old").add_label("Tall")"#,
            ),
            (
                "MATCH (s:software) REMOVE s:software",
                r#"g.v().has_label("software").drop_label("software")"#,
            ),
            (
                "MATCH (p:person) SET p += {a: 1, b: 'x'}",
                r#"g.v().has_label("person").property(#{a: 1, b: "x"})"#,
            ),
            (
                "MATCH (p {name: 'marko'}) SET p = {x: 1}",
                r#"g.v().has("name", "marko").remove_property(["name", "age"]).property("x", 1)"#,
            ),
            (
                "MATCH ()-[e:knows]->() SET e.weight = 2.5",
                r#"g.e().has_label("knows").property("weight", 2.5)"#,
            ),
            (
                "MATCH (a)-[e:knows]->(b) REMOVE e.weight",
                r#"g.e().has_label("knows").remove_property("weight")"#,
            ),
            (
                "MATCH (p:person) CALL (p) { SET p.k = 1 } RETURN p",
                r#"g.v().has_label("person").property("k", 1)"#,
            ),
            (
                "MATCH (a)-[:created]->(s) SET s.by = 'x', a:Maker",
                r#"g.v().as("a").out("created").property("by", "x").select("a").add_label("Maker")"#,
            ),
        ] {
            twin(query, gremlin);
        }
    }

    #[test]
    fn a_gql_set_or_remove_is_mutating() {
        for query in [
            "MATCH (p) SET p.k = 1",
            "MATCH (p) SET p:A RETURN p",
            "MATCH (p) REMOVE p.age",
            "MATCH (p) CALL (p) { REMOVE p:person } RETURN p",
        ] {
            let traversal = eval_value(modern(), &format!("g.gql({query:?})"))
                .unwrap()
                .cast::<ScriptGraphTraversalSource>();
            assert!(traversal.is_mutating(), "{query}");
        }
    }

    /// A saved GQL query with `SET` / `REMOVE` writes (dialect `/2`) with `Update` effects.
    #[test]
    fn a_saved_gql_query_with_set_writes_with_update_effects() {
        use graphersal::catalog::GQL_WRITING_QUERY_DIALECT;
        let graph = modern();
        let text = |script: &str| {
            eval_value(graph.clone(), script)
                .unwrap_or_else(|e| panic!("{script}: {e}"))
                .to_string()
        };
        let _ = text(
            r#"g.define_query(#{name: "bump", language: "gql", params: #{n: "string"},
                   body: "MATCH (p:person {name: $n}) SET p.age = p.age + 1, p:Old RETURN p.age AS age"});
               g.define_query(#{name: "unweigh", language: "gql",
                   body: "MATCH (a)-[e:knows]->(b) REMOVE e.weight"})"#,
        );
        assert_eq!(
            text(r#"g.get_query("bump").dialect"#),
            GQL_WRITING_QUERY_DIALECT
        );
        assert_eq!(text(r#"g.get_query("bump").writes"#), "true");
        assert_eq!(
            text(r#"g.get_query("bump").effects.map(|e| e.text)"#),
            r#"["Update Data(vertex)"]"#
        );
        assert_eq!(
            text(r#"g.get_query("unweigh").effects.map(|e| e.text)"#),
            r#"["Update Data(edge \"knows\")"]"#
        );
        assert_eq!(
            text(r#"g.query("bump", #{n: "marko"}).select("age").to_list()"#),
            "[30]"
        );
        let _ = text(r#"g.query("unweigh").iterate()"#);
        let read = graph.read();
        let lines = dump(&*read);
        assert!(
            lines
                .iter()
                .any(|l| l == "V [\"person\", \"Old\"] {age=30, name=\"marko\"}"),
            "{lines:?}"
        );
        assert!(
            lines.iter().filter(|l| l.starts_with("E knows {}")).count() == 2,
            "{lines:?}"
        );
    }

    /// Stage 3d: `DELETE` / `DETACH DELETE` leave the graph their `drop()` twins leave.
    #[test]
    fn delete_leaves_the_graph_its_drop_twin_leaves() {
        for (query, gremlin) in [
            (
                "MATCH (n:person {name: 'marko'}) DETACH DELETE n",
                r#"g.v().has("name", "marko").drop()"#,
            ),
            (
                "MATCH (n:software) DETACH DELETE n",
                r#"g.v().has_label("software").drop()"#,
            ),
            (
                "MATCH ()-[r:knows]->() DELETE r",
                r#"g.e().has_label("knows").drop()"#,
            ),
            (
                "MATCH (n {name: 'vadas'})-[r]-() DELETE r, n",
                r#"g.v().has("name", "vadas").side_effect(__.both_e().drop()).drop()"#,
            ),
            (
                "MATCH (n)-[r]-() DELETE n, r",
                r#"g.v().side_effect(__.both_e().drop()).drop()"#,
            ),
            (
                "MATCH p = (:person {name: 'vadas'})<-[]-() DETACH DELETE p",
                r#"g.v().has("name", "vadas").in_e().out_v().path().unfold().drop()"#,
            ),
            (
                "MATCH (s:software) WITH collect(s) AS l DETACH DELETE l",
                r#"g.v().has_label("software").fold().unfold().drop()"#,
            ),
            (
                "MATCH (p:person) CALL (p) { MATCH (p)-[e:created]->() DELETE e } RETURN p",
                r#"g.v().has_label("person").out_e("created").drop()"#,
            ),
        ] {
            twin(query, gremlin);
        }
    }

    #[test]
    fn a_gql_delete_is_mutating() {
        for query in [
            "MATCH (p) DETACH DELETE p",
            "MATCH (p) DELETE p RETURN count(p)",
            "MATCH ()-[e]->() DELETE e",
            "MATCH (p) CALL (p) { DETACH DELETE p } RETURN 1",
            "MATCH (p) DELETE NULL",
        ] {
            let traversal = eval_value(modern(), &format!("g.gql({query:?})"))
                .unwrap()
                .cast::<ScriptGraphTraversalSource>();
            assert!(traversal.is_mutating(), "{query}");
        }
    }

    /// A saved GQL query with `DELETE` writes (dialect `/2`) with `Delete` effects, those of its
    /// `drop()` twin.
    #[test]
    fn a_saved_gql_query_with_delete_writes_with_delete_effects() {
        use graphersal::catalog::GQL_WRITING_QUERY_DIALECT;
        let graph = modern();
        let text = |script: &str| {
            eval_value(graph.clone(), script)
                .unwrap_or_else(|e| panic!("{script}: {e}"))
                .to_string()
        };
        let _ = text(
            r#"g.define_query(#{name: "forget", language: "gql", params: #{n: "string"},
                   body: "MATCH (p:person {name: $n}) DETACH DELETE p"});
               g.define_query(#{name: "unlink", language: "gql",
                   body: "MATCH ()-[e:knows]->() DELETE e RETURN count(*) AS c"});
               g.define_query(#{name: "drop_people", body: `fn drop_people() { g.v().has_label("person").drop() }`})"#,
        );
        assert_eq!(
            text(r#"g.get_query("forget").dialect"#),
            GQL_WRITING_QUERY_DIALECT
        );
        assert_eq!(text(r#"g.get_query("forget").writes"#), "true");
        assert_eq!(
            text(r#"g.get_query("forget").effects.map(|e| e.text)"#),
            r#"["Delete Data(vertex)"]"#
        );
        assert_eq!(
            text(r#"g.get_query("drop_people").effects.map(|e| e.text)"#),
            r#"["Delete Data(vertex)"]"#
        );
        assert_eq!(
            text(r#"g.get_query("unlink").effects.map(|e| e.text)"#),
            r#"["Delete Data(edge \"knows\")"]"#
        );
        assert_eq!(text(r#"g.query("unlink").select("c").to_list()"#), "[2]");
        let _ = text(r#"g.query("forget", #{n: "marko"}).iterate()"#);
        let read = graph.read();
        // marko's `knows` edges went with `unlink`, his `created` edge with him.
        assert_eq!((read.vertex_count(), read.edge_count()), (5, 3));
    }
    /// The GQL `query` and the Gremlin `gremlin` leave the same graph (a MERGE that only
    /// matches may leave the graph as it was).
    fn same_state(query: &str, gremlin: &str) {
        let gql = state(&format!("g.gql({query:?}).iterate()"));
        let twin = state(&format!("{gremlin}.iterate()"));
        assert_eq!(gql, twin, "{query}\nvs {gremlin}");
    }

    /// Stage 3e: MERGE leaves the graph its Gremlin twins leave where label and equality
    /// semantics agree: `merge_v()` over injected maps (per traverser, W34), `merge_v(map)` with
    /// `Merge.onMatch` / `Merge.onCreate` (an `onCreate` map holding only new keys adds them, as
    /// `ON CREATE SET`), `merge_e()` between bound ends (`Merge.outV` / `Merge.inV`), the
    /// `fold().coalesce(__.unfold(), __.add_v(..))` idiom and its per-row form
    /// `coalesce(.. where(P.eq("x")).by("k").by(), add_v(..))`, and an undirected match through
    /// `both_e()` + `other_v()`.
    #[test]
    fn merge_leaves_the_graph_its_gremlin_twins_leave() {
        for (query, gremlin) in [
            (
                "FOR x IN [1, 1, 2] MERGE (:n {k: x})",
                r#"g.inject(#{"T.label": "n", k: 1}, #{"T.label": "n", k: 1}, #{"T.label": "n", k: 2}).merge_v()"#,
            ),
            (
                "MERGE (p:person {name: 'marko'}) ON MATCH SET p.age = 30",
                r#"g.merge_v(#{"T.label": "person", name: "marko"}).option(Merge.onMatch, #{age: 30})"#,
            ),
            (
                "MERGE (p:person {name: 'q'}) ON CREATE SET p.age = 1 ON MATCH SET p.age = 2",
                r#"g.merge_v(#{"T.label": "person", name: "q"}).option(Merge.onCreate, #{age: 1}).option(Merge.onMatch, #{age: 2})"#,
            ),
            (
                "MERGE (p:person) ON MATCH SET p.seen = true",
                r#"g.merge_v(#{"T.label": "person"}).option(Merge.onMatch, #{seen: true})"#,
            ),
            (
                "MATCH (a {name: 'marko'}), (b {name: 'lop'}) MERGE (a)-[:likes]->(b)",
                r#"g.v("1").as("a").v("3").as("b").merge_e(#{"T.label": "likes", "Direction.OUT": Merge.outV, "Direction.IN": Merge.inV}).option(Merge.outV, __.select("a")).option(Merge.inV, __.select("b"))"#,
            ),
            (
                "MATCH (a {name: 'marko'}), (b {name: 'lop'}) MERGE (a)-[e:created {weight: 0.4}]->(b) \
                 ON MATCH SET e.w = 2",
                r#"g.v("1").as("a").v("3").as("b").merge_e(#{"T.label": "created", weight: 0.4, "Direction.OUT": Merge.outV, "Direction.IN": Merge.inV}).option(Merge.outV, __.select("a")).option(Merge.inV, __.select("b")).option(Merge.onMatch, #{w: 2})"#,
            ),
            (
                "MATCH (a {name: 'josh'}), (b {name: 'marko'}) MERGE (a)-[e:knows]->(b) \
                 ON CREATE SET e.w = 1 ON MATCH SET e.w = 2",
                r#"g.v("4").as("a").v("1").as("b").merge_e(#{"T.label": "knows", "Direction.OUT": Merge.outV, "Direction.IN": Merge.inV}).option(Merge.outV, __.select("a")).option(Merge.inV, __.select("b")).option(Merge.onCreate, #{w: 1}).option(Merge.onMatch, #{w: 2})"#,
            ),
            (
                "MERGE (:person {name: 'marko'})",
                r#"g.v().has("person", "name", "marko").fold().coalesce(__.unfold(), __.add_v("person").property("name", "marko"))"#,
            ),
            (
                "MERGE (:person {name: 'zed'})",
                r#"g.v().has("person", "name", "zed").fold().coalesce(__.unfold(), __.add_v("person").property("name", "zed"))"#,
            ),
            (
                "FOR x IN [1, 1, 2] MERGE (:n {k: x})",
                r#"g.inject(1, 1, 2).as("x").coalesce(__.v().has_label("n").where(P.eq("x")).by("k").by(), __.add_v("n").property("k", __.select("x")))"#,
            ),
            (
                "MATCH (a {name: 'josh'}), (b {name: 'marko'}) MERGE (a)-[:knows]-(b)",
                r#"g.v("4").as("a").v("1").as("b").coalesce(__.select("a").both_e("knows").where(__.other_v().as("b")), __.add_e("knows").from("a").to("b"))"#,
            ),
            (
                "MATCH (a {name: 'josh'}), (b {name: 'vadas'}) MERGE (a)-[:knows]-(b)",
                r#"g.v("4").as("a").v("2").as("b").coalesce(__.select("a").both_e("knows").where(__.other_v().as("b")), __.add_e("knows").from("a").to("b"))"#,
            ),
        ] {
            same_state(query, gremlin);
        }
        // The creating rows change the graph (the twins above are no empty comparison).
        twin(
            "FOR x IN [1, 1, 2] MERGE (:n {k: x})",
            r#"g.inject(#{"T.label": "n", k: 1}, #{"T.label": "n", k: 1}, #{"T.label": "n", k: 2}).merge_v()"#,
        );
        twin(
            "MATCH (a {name: 'josh'}), (b {name: 'vadas'}) MERGE (a)-[:knows]-(b)",
            r#"g.v("4").as("a").v("2").as("b").add_e("knows").from("a").to("b")"#,
        );
    }

    #[test]
    fn a_gql_merge_is_mutating() {
        for query in [
            "MERGE (n:N)",
            "MERGE (p:person {name: 'marko'}) RETURN p",
            "MATCH (a), (b) MERGE (a)-[:r]->(b)",
            "MERGE (p:person) ON MATCH SET p.k = 1",
            "MATCH (p) CALL (p) { MERGE (p)-[:r]->(:x) } RETURN p",
        ] {
            let traversal = eval_value(modern(), &format!("g.gql({query:?})"))
                .unwrap()
                .cast::<ScriptGraphTraversalSource>();
            assert!(traversal.is_mutating(), "{query}");
        }
    }

    /// A saved GQL query with MERGE writes (dialect `/2`) with the `Create` effects of what its
    /// pattern can create and, with ON items, their `Update` effects.
    #[test]
    fn a_saved_gql_query_with_merge_writes_with_create_and_update_effects() {
        use graphersal::catalog::GQL_WRITING_QUERY_DIALECT;
        let graph = modern();
        let text = |script: &str| {
            eval_value(graph.clone(), script)
                .unwrap_or_else(|e| panic!("{script}: {e}"))
                .to_string()
        };
        let _ = text(
            r#"g.define_query(#{name: "visit", language: "gql", params: #{n: "string"},
                   body: "MERGE (p:person {name: $n}) ON CREATE SET p.visits = 1 ON MATCH SET p.visits = coalesce(p.visits, 0) + 1 RETURN p.visits AS v"});
               g.define_query(#{name: "link", language: "gql", params: #{a: "string", b: "string"},
                   body: "MATCH (x:person {name: $a}), (y:person {name: $b}) MERGE (x)-[:knows]->(y)"})"#,
        );
        assert_eq!(
            text(r#"g.get_query("visit").dialect"#),
            GQL_WRITING_QUERY_DIALECT
        );
        assert_eq!(text(r#"g.get_query("visit").writes"#), "true");
        assert_eq!(
            text(r#"g.get_query("visit").effects.map(|e| e.text)"#),
            r#"["Create Data(vertex \"person\")", "Update Data(vertex)"]"#
        );
        assert_eq!(
            text(r#"g.get_query("link").effects.map(|e| e.text)"#),
            r#"["Create Data(edge \"knows\")"]"#
        );
        assert_eq!(
            text(r#"g.query("visit", #{n: "zed"}).select("v").to_list()"#),
            "[1]"
        );
        assert_eq!(
            text(r#"g.query("visit", #{n: "zed"}).select("v").to_list()"#),
            "[2]"
        );
        let _ = text(r#"g.query("link", #{a: "marko", b: "josh"}).iterate()"#);
        let _ = text(r#"g.query("link", #{a: "josh", b: "marko"}).iterate()"#);
        let read = graph.read();
        assert_eq!((read.vertex_count(), read.edge_count()), (7, 7));
    }

    /// Elements a MERGE creates (top level and in a `CALL` body) carry the evaluation time of
    /// the script (`defaultFunction: now`), as INSERT does; a match creates nothing.
    #[test]
    fn merged_elements_carry_the_evaluation_time() {
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
                r#"g.gql("FOR i IN [1, 2, 1] MERGE (:event {i: i})-[:next]->(:event {j: i})").iterate();
                   g.gql("MATCH (e:event {i: 1}) CALL (e) { MERGE (e)-[:next]->(:event {k: 1}) } RETURN e", #{}).iterate()"#,
            )
            .unwrap();
        graphersal::script::pin_now(&mut engine, None);
        let read = graph.read();
        let mut stamps = 0;
        for v in read.vertices().unwrap() {
            let at = read
                .vertex_properties(v)
                .unwrap()
                .and_then(|p| p.get("at"))
                .cloned();
            assert_eq!(
                at,
                Some(ElementProperty::Temporal(Temporal::DateTime(pinned)))
            );
            stamps += 1;
        }
        for e in read.edges().unwrap() {
            let at = read
                .edge_properties(e)
                .unwrap()
                .and_then(|p| p.get("at"))
                .cloned();
            assert_eq!(
                at,
                Some(ElementProperty::Temporal(Temporal::DateTime(pinned)))
            );
            stamps += 1;
        }
        // Rows 1 and 2 create two events and an edge each, row 3 matches; the CALL body adds
        // one event and one edge.
        assert_eq!(stamps, 8);
    }
}

// ----- stage 3c: SET / REMOVE -----

/// The persons' column `key` on `graph`, ordered by name (josh, marko, peter, vadas).
fn persons(graph: &mut TraversalGraph, key: &str) -> Vec<String> {
    let rows = write(
        graph,
        &format!("MATCH (p:person) RETURN p.name AS n, p.{key} AS v ORDER BY n"),
    );
    column(&rows, "v")
}

#[test]
fn set_and_remove_return_the_rows_with_the_new_state() {
    // RETURN after SET reads the new values; the row and its element stay.
    let (mut graph, rows) = after(
        "MATCH (p:person) SET p.age = p.age + 1, p:Checked \
         RETURN p.name AS n, p.age AS a, labels(p) AS l ORDER BY n",
    );
    assert_eq!(column(&rows, "a"), ["33", "30", "36", "28"]);
    assert_eq!(count_label(&graph, "Checked"), 4);
    assert_eq!(column(&rows, "l")[0], r#"["person", "Checked"]"#);
    // REMOVE of a property and a label; a missing key or label is a no-op.
    let rows = write(
        &mut graph,
        "MATCH (p:person) REMOVE p.age, p.nothing, p:Checked, p:Nothing RETURN p.age AS a",
    );
    assert_eq!(column(&rows, "a"), ["null", "null", "null", "null"]);
    assert_eq!(count_label(&graph, "Checked"), 0);
    assert_eq!(count_label(&graph, "person"), 4);
    // A query ending in SET / REMOVE returns no row.
    assert!(write(&mut graph, "MATCH (p:person) SET p.k = 1").is_empty());
    assert!(write(&mut graph, "MATCH (p:person) REMOVE p.k").is_empty());
    assert_eq!(persons(&mut graph, "k"), ["null", "null", "null", "null"]);
}

#[test]
fn set_items_run_left_to_right_and_null_removes() {
    let (mut graph, rows) = after(
        "MATCH (p {name: 'marko'}) SET p.a = 1, p.b = p.a + 1, p.a = 3, (p).c = p.b \
         RETURN p.a AS a, p.b AS b, p.c AS c",
    );
    assert_eq!(column(&rows, "a"), ["3"]);
    assert_eq!(column(&rows, "b"), ["2"]);
    assert_eq!(column(&rows, "c"), ["2"]);
    // `= NULL` removes the key, `= {..}` replaces every property (NULL entries absent), `+=`
    // merges (a NULL entry removes its key).
    write(&mut graph, "MATCH (p {name: 'marko'}) SET p.a = NULL");
    let rows = write(
        &mut graph,
        "MATCH (p {name: 'marko'}) RETURN keys(p) AS k ORDER BY k",
    );
    assert_eq!(column(&rows, "k"), [r#"["name", "age", "b", "c"]"#]);
    let rows = write(
        &mut graph,
        "MATCH (p {name: 'vadas'}) SET p = {x: 1, y: NULL} RETURN properties(p) AS m",
    );
    assert_eq!(column(&rows, "m"), [r#"{"x": 1}"#]);
    let rows = write(
        &mut graph,
        "MATCH (p {x: 1}) SET p += {x: NULL, z: 'z', w: [1, 2]} RETURN properties(p) AS m",
    );
    assert_eq!(column(&rows, "m"), [r#"{"z": "z", "w": [1, 2]}"#]);
    // A NULL target (OPTIONAL MATCH without a match) is a no-op whatever its right side.
    let before = dump(&graph);
    let rows = write(
        &mut graph,
        "OPTIONAL MATCH (a:Nothing) SET a.k = 1 / 0, a:L REMOVE a.k, a:L RETURN a",
    );
    assert_eq!(column(&rows, "a"), ["null"]);
    assert_eq!(dump(&graph), before);
}

/// Design W10: a value read across a SET is the value from before it, also through a list of
/// the elements (TCK List12 shape).
#[test]
fn a_value_read_across_a_set_is_the_value_from_before_it() {
    let (mut graph, rows) = after(
        "MATCH (n:person) WITH n, n.age AS old SET n.age = 1 \
         RETURN n.name AS n, old ORDER BY n",
    );
    assert_eq!(column(&rows, "old"), ["32", "29", "35", "27"]);
    assert_eq!(persons(&mut graph, "age"), ["1", "1", "1", "1"]);
    let (mut graph, rows) = after(
        "MATCH (a:person) WITH collect(a) AS nodes \
         WITH nodes, [x IN nodes | x.name] AS oldNames UNWIND nodes AS n \
         SET n.name = n.name + '!' RETURN n.name AS new, oldNames ORDER BY new LIMIT 1",
    );
    assert_eq!(column(&rows, "new"), ["\"josh!\""]);
    assert_eq!(
        column(&rows, "oldNames"),
        [r#"["marko", "vadas", "josh", "peter"]"#]
    );
    assert_eq!(
        persons(&mut graph, "name"),
        ["\"josh!\"", "\"marko!\"", "\"peter!\"", "\"vadas!\""]
    );
}

/// Design W15: every row is one evaluation, also over duplicate rows of the same element; a
/// `LIMIT 0` after the write limits the rows, not the writes (TCK Set6 / Remove3).
#[test]
fn every_row_sets_once_also_over_duplicate_rows() {
    let (mut graph, _) = after("MATCH (n {name: 'marko'}) SET n.c = 0");
    write(
        &mut graph,
        "MATCH (n {name: 'marko'}), (m:person) SET n.c = n.c + 1",
    );
    let rows = write(&mut graph, "MATCH (n {name: 'marko'}) RETURN n.c AS c");
    assert_eq!(column(&rows, "c"), ["4"]);
    write(
        &mut graph,
        "FOR i IN [1, 2, 3] MATCH (n {name: 'marko'}) SET n.c = n.c + i",
    );
    let rows = write(&mut graph, "MATCH (n {name: 'marko'}) RETURN n.c AS c");
    assert_eq!(column(&rows, "c"), ["10"]);
    let (mut graph, rows) = after("MATCH (n:person) SET n.k = 1 RETURN n LIMIT 0");
    assert!(rows.is_empty());
    assert_eq!(persons(&mut graph, "k"), ["1", "1", "1", "1"]);
    let (graph, rows) = after("MATCH (n:person) REMOVE n:person RETURN count(*) AS c");
    assert_eq!(column(&rows, "c"), ["4"]);
    assert_eq!(count_label(&graph, "person"), 0);
}

/// A later clause sees the SET: a MATCH after it, also one an index serves (the index follows
/// the write), and a query after the write.
#[test]
fn a_later_match_sees_the_set_also_through_a_property_index() {
    use graphersal::catalog::PropertyIndexDefinition;
    use graphersal::storage::ElementKind;
    let (_, rows) = after(
        "MATCH (n {name: 'marko'}) SET n.age = 77 WITH n MATCH (m:person {age: 77}) \
         RETURN m.name AS m",
    );
    assert_eq!(column(&rows, "m"), ["\"marko\""]);
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .create_index(
            "person_age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        )
        .unwrap();
    write(&mut graph, "MATCH (n {name: 'marko'}) SET n.age = 77");
    let query = "MATCH (p:person {age: 77}) RETURN p.name AS n";
    assert_eq!(column(&write(&mut graph, query), "n"), ["\"marko\""]);
    assert!(write(&mut graph, "MATCH (p:person {age: 29}) RETURN p").is_empty());
    let plan = plan_dump(&graph, query);
    assert!(
        plan.starts_with("v(labels: [\"person\"]) [index: vertex person.age]\n"),
        "{plan}"
    );
    write(&mut graph, "MATCH (n {name: 'marko'}) REMOVE n.age");
    assert!(write(&mut graph, query).is_empty());
}

#[test]
fn set_and_remove_lower_to_one_step_per_clause() {
    assert_eq!(
        write_plan("MATCH (p:person) SET p.age = 1, p:A RETURN p", &[]),
        "v(labels: [\"person\"])\ngql:set(\"p.age = 1, p:A\")\ngql:project(\"p\")\n"
    );
    assert_eq!(
        write_plan("MATCH (p:person) REMOVE p.age, p:A", &[]),
        "v(labels: [\"person\"])\ngql:remove(\"p.age, p:A\")\ndiscard()\n"
    );
    // A target away from the current element is read through its path label.
    let plan = write_plan("MATCH (a)-[e:knows]->(b) SET a.k = e.weight RETURN b", &[]);
    assert!(
        plan.contains("as(\"e\") [path: labels(a,e)]\nin_v() [path: labels(a,e)]\ngql:set(\"a.k = e.weight\")\n"),
        "{plan}"
    );
}

#[test]
fn set_inside_a_call_body_runs_per_row_and_the_rows_pass_on() {
    let (mut graph, rows) = after(
        "MATCH (p:person) CALL (p) { SET p.k = 1 REMOVE p.age } RETURN p.name AS n ORDER BY n",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(persons(&mut graph, "k"), ["1", "1", "1", "1"]);
    assert_eq!(persons(&mut graph, "age"), ["null", "null", "null", "null"]);
    let (mut graph, _) = after(
        "MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:created]->(s) SET s.by = p.name } \
         RETURN p",
    );
    let rows = write(
        &mut graph,
        "MATCH (s:software) RETURN s.name AS n, s.by AS b ORDER BY n",
    );
    assert_eq!(column(&rows, "b").len(), 2);
}

#[test]
fn the_schema_coerces_and_refuses_set_values() {
    let mut graph = TraversalGraph::new();
    graph
        .set_schema(
            GraphSchema::from_json(
                r#"{"mode": "closed",
                  "vertices": {"person": {"schema": {"type": "object", "properties": {
                      "name": {"type": "string"}, "w": {"type": "number"}},
                    "required": ["name"]}}}}"#,
            )
            .unwrap(),
        )
        .unwrap();
    write(
        &mut graph,
        "INSERT (:person {name: 'ann'}), (:person {name: 'bob'})",
    );
    // Write-time coercion: an integer for a number is stored as a float.
    write(&mut graph, "MATCH (p:person) SET p.w = 2");
    for v in graph.vertices().unwrap() {
        let w = graph
            .vertex_properties(v)
            .unwrap()
            .and_then(|p| p.get("w"))
            .cloned();
        assert_eq!(w, Some(ElementProperty::from(2.0)));
    }
    let before = dump(&graph);
    for query in [
        // A value of the wrong type, an undeclared key, a required key removed: the query
        // fails and changes nothing, also for the rows before the failing one.
        "MATCH (p:person) SET p.w = p.name",
        "MATCH (p:person) SET p.other = 1",
        "MATCH (p:person) REMOVE p.name",
        "MATCH (p:person) SET p.name = NULL",
        "MATCH (p:person) SET p = {w: 1}",
    ] {
        let err = graph.traversal_mut().gql(query).to_list().unwrap_err();
        assert!(
            matches!(
                err.root_cause(),
                TraverserError::Graph(GraphError::Schema(_)) | TraverserError::Schema(_)
            ),
            "{query}: {err:?}"
        );
        assert_eq!(dump(&graph), before, "{query} changed the graph");
    }
}

#[test]
fn a_failing_set_row_changes_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = dump(&graph);
    for query in [
        "MATCH (p:person) SET p.k = 1, p.r = 100 / (p.age - 35)",
        "MATCH (p:person) SET p:Seen REMOVE p.name SET p.k = 1 / (p.age - 35)",
        "FOR x IN [1, 'a'] MATCH (p:person) SET p = x",
    ] {
        assert!(
            graph.traversal_mut().gql(query).to_list().is_err(),
            "{query}"
        );
        assert_eq!(dump(&graph), before, "{query} changed the graph");
    }
}

// ----- stage 3d: DELETE / DETACH DELETE -----

/// The root cause of the failed `query` on `graph` (the graph left as it was is checked).
fn delete_error(graph: &mut TraversalGraph, query: &str) -> Box<TraverserError> {
    let before = dump(graph);
    let error = graph.traversal_mut().gql(query).to_list().expect_err(query);
    assert_eq!(dump(graph), before, "{query} changed the graph");
    error
}

#[test]
fn detach_delete_of_a_hub_takes_its_edges_and_returns_no_row() {
    let (graph, rows) = after("MATCH (n:person {name: 'marko'}) DETACH DELETE n");
    assert!(rows.is_empty());
    assert_eq!((graph.vertex_count(), graph.edge_count()), (5, 3));
    assert_eq!(count_edges(&graph, "knows"), 0);
    // A node without edges goes with a plain DELETE (NODETACH is the default).
    let mut graph = TraversalGraph::tinkerpop_modern();
    write(&mut graph, "INSERT (:Lonely), (:Lonely)");
    write(&mut graph, "MATCH (n:Lonely) DELETE n");
    write(&mut graph, "INSERT (:Lonely)");
    write(&mut graph, "MATCH (n:Lonely) NODETACH DELETE n");
    assert_eq!(dump(&graph), dump(&TraversalGraph::tinkerpop_modern()));
}

/// Design W29: the NODETACH check is per clause, so an edge and its ends go in one clause; a
/// node with an edge the clause does not delete is G1001 and nothing is removed.
#[test]
fn the_nodetach_check_is_per_clause_and_a_failure_leaves_nothing() {
    let (graph, _) = after("MATCH (n)-[r]-() DELETE n, r");
    assert_eq!((graph.vertex_count(), graph.edge_count()), (0, 0));
    let (graph, rows) = after("MATCH (a)-[r]-(b) DELETE r, a, b RETURN count(*) AS c");
    assert_eq!(column(&rows, "c"), ["12"]);
    assert_eq!((graph.vertex_count(), graph.edge_count()), (0, 0));
    let mut graph = TraversalGraph::tinkerpop_modern();
    for query in [
        "MATCH (n:person {name: 'marko'}) DELETE n",
        "MATCH (n:software) DELETE n RETURN count(*) AS c",
        "MATCH (n {name: 'marko'})-[r:knows]->() DELETE n, r",
        // Deleting the edges in a later clause does not help (the check is per clause).
        "MATCH (n {name: 'vadas'})-[r]-() DELETE n DELETE r",
        "MATCH p = (:person {name: 'marko'})-[:knows]->() DELETE p",
    ] {
        let error = delete_error(&mut graph, query);
        match error.root_cause() {
            TraverserError::Gql {
                kind, gqlstatus, ..
            } => {
                assert_eq!(*kind, GqlErrorKind::Data, "{query}");
                assert_eq!(gqlstatus, "G1001", "{query}");
            }
            other => panic!("{query}: {other:?}"),
        }
        let help = error.help().unwrap_or_default();
        assert!(help.contains("DETACH DELETE"), "{query}: {help}");
    }
}

#[test]
fn delete_takes_paths_lists_and_ignores_null_and_removed_targets() {
    let (graph, _) = after("MATCH p = (:person {name: 'vadas'})<-[]-() DETACH DELETE p");
    assert_eq!((graph.vertex_count(), graph.edge_count()), (4, 3));
    let mut graph = TraversalGraph::new();
    write(&mut graph, "INSERT (:A)-[:R]->(:B)-[:R]->(:C)");
    write(&mut graph, "MATCH p = (:A)-->()-->(:C) DELETE p");
    assert_eq!((graph.vertex_count(), graph.edge_count()), (0, 0));
    // Lists, nested lists and NULL items.
    let (graph, _) = after("MATCH (s:software) WITH collect(s) AS l DETACH DELETE l");
    assert_eq!((graph.vertex_count(), graph.edge_count()), (4, 2));
    let (graph, _) =
        after("MATCH (s:software {name: 'lop'}) DETACH DELETE [[s], NULL, [NULL, [s]]]");
    assert_eq!((graph.vertex_count(), graph.edge_count()), (5, 3));
    // NULL targets are no-ops; the row still comes out.
    let (graph, rows) = after("OPTIONAL MATCH (n:Nothing) DELETE n RETURN n");
    assert_eq!(column(&rows, "n"), ["null"]);
    assert_eq!(dump(&graph), dump(&TraversalGraph::tinkerpop_modern()));
    let (graph, _) = after("MATCH (n) DELETE NULL");
    assert_eq!(dump(&graph), dump(&TraversalGraph::tinkerpop_modern()));
    // Twice: in several rows, twice in one clause, again in a later clause.
    for query in [
        "MATCH (n:software), (m:person) DETACH DELETE n",
        "MATCH (n:software) DETACH DELETE n, n",
        "MATCH (n:software) DETACH DELETE n DETACH DELETE n",
        "MATCH (n:software)<-[r]-() DELETE r DETACH DELETE n",
    ] {
        let (graph, _) = after(query);
        assert_eq!(
            (graph.vertex_count(), graph.edge_count()),
            (4, 2),
            "{query}"
        );
        assert_eq!(count_label(&graph, "software"), 0, "{query}");
    }
}

/// `count(n)` and `n IS NULL` of a deleted node still work; the rows after a DELETE are the
/// rows before it, also under `LIMIT 0` (W13), and a value read across it is the value from
/// before it (W10, TCK Delete6).
#[test]
fn rows_pass_a_delete_and_values_read_before_it_survive() {
    let (graph, rows) =
        after("MATCH (n:software) DETACH DELETE n RETURN count(n) AS c, n IS NULL AS z");
    assert_eq!(column(&rows, "c"), ["2"]);
    assert_eq!(column(&rows, "z"), ["false"]);
    assert_eq!(graph.vertex_count(), 4);
    let (graph, rows) = after("MATCH (n:software) DETACH DELETE n RETURN 42 AS num LIMIT 0");
    assert!(rows.is_empty());
    assert_eq!(graph.vertex_count(), 4);
    let (graph, rows) = after(
        "MATCH (n:software) WITH n, n.name AS name DETACH DELETE n RETURN name ORDER BY name",
    );
    assert_eq!(column(&rows, "name"), ["\"lop\"", "\"ripple\""]);
    assert_eq!(graph.vertex_count(), 4);
    let (_, rows) =
        after("MATCH (n:person) WITH n, n.age AS age DETACH DELETE n RETURN sum(age) AS s");
    assert_eq!(column(&rows, "s"), ["123"]);
    // A later MATCH no longer finds the deleted elements.
    let (_, rows) = after(
        "MATCH (n:software) DETACH DELETE n WITH count(*) AS c MATCH (m) RETURN count(m) AS m",
    );
    assert_eq!(column(&rows, "m"), ["4"]);
}

/// Design W27 / W28 (user decision 5): reading or returning an element deleted earlier in the
/// query is an error with the root cause `ElementRemoved` and a GQL help; `type(r)` of a
/// deleted edge reads its label (E3, TCK Return2 [14]).
#[test]
fn reading_or_returning_a_deleted_element_is_an_error() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for query in [
        "MATCH (n:software) DETACH DELETE n RETURN n.name",
        "MATCH (n:software) DETACH DELETE n RETURN labels(n)",
        "MATCH (n:software) DETACH DELETE n RETURN element_id(n)",
        "MATCH (n:software) DETACH DELETE n RETURN properties(n)",
        "MATCH ()-[r:created]->() DELETE r RETURN r.weight",
        "MATCH (n:software) DETACH DELETE n SET n.k = 1",
        // W28: the element itself, also inside a list or a path, also after a WITH.
        "MATCH (n:software) DETACH DELETE n RETURN n",
        "MATCH (n:software) DETACH DELETE n RETURN [n] AS l",
        "MATCH ()-[r:created]->() DELETE r RETURN r",
        "MATCH p = (:software)<--() DETACH DELETE p RETURN p",
        "MATCH (n:software) DETACH DELETE n WITH n RETURN n",
    ] {
        let error = delete_error(&mut graph, query);
        assert!(
            matches!(
                error.root_cause(),
                TraverserError::Graph(GraphError::ElementRemoved { .. })
            ),
            "{query}: {error:?}"
        );
        let help = error.help().unwrap_or_default();
        assert!(
            help.contains("WITH n, properties(n) AS p DETACH DELETE n RETURN p"),
            "{query}: {help}"
        );
    }
    let (_, rows) = after("MATCH ()-[r:created]->() DELETE r RETURN type(r) AS t");
    assert_eq!(column(&rows, "t"), ["\"created\""; 4]);
    let (_, rows) = after(
        "MATCH (n:software) WITH n, properties(n) AS p DETACH DELETE n RETURN p.name AS m \
         ORDER BY m",
    );
    assert_eq!(column(&rows, "m"), ["\"lop\"", "\"ripple\""]);
}

#[test]
fn delete_lowers_to_one_step_per_clause_after_the_materialization() {
    assert_eq!(
        write_plan("MATCH (n:software) DETACH DELETE n", &[]),
        "v(labels: [\"software\"])\ngql:delete(\"DETACH DELETE n\")\ndiscard()\n"
    );
    assert_eq!(
        write_plan(
            "MATCH (a)-[r:knows]->(b) DELETE r DETACH DELETE a, b RETURN count(*) AS c",
            &[]
        ),
        "v()\nas(\"a\") [path: labels(a)]\nout_e(\"knows\") [path: labels(a)]\n\
         as(\"r\") [path: labels(a,r)]\nin_v() [path: labels(a,r)]\n\
         gql:delete(\"DELETE r\") [path: labels(a)]\ngql:delete(\"DETACH DELETE a, b\")\n\
         gql:aggregate([], [\"count(*)\"], [\"c\"])\n"
    );
}

#[test]
fn delete_inside_a_call_body_runs_per_row_and_the_rows_pass_on() {
    let (graph, rows) = after(
        "MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) DETACH DELETE s } \
         RETURN p.name AS n ORDER BY n",
    );
    assert_eq!(column(&rows, "n").len(), 4);
    assert_eq!((graph.vertex_count(), graph.edge_count()), (4, 2));
    let (graph, _) = after("MATCH (p:person {name: 'vadas'}) CALL (p) { DETACH DELETE p }");
    assert_eq!((graph.vertex_count(), graph.edge_count()), (5, 5));
}

#[test]
fn a_failing_delete_row_changes_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for query in [
        "MATCH (p:person) DETACH DELETE p RETURN 100 / (p.age - 35)",
        "MATCH (p:person) WITH p, 1 / (p.age - 35) AS x DETACH DELETE p",
        "MATCH (p:person) WITH p.name AS x DELETE x",
    ] {
        let _ = delete_error(&mut graph, query);
    }
}

// ----- stage 3e: MERGE end to end (3e.T3 lowering, 3e.T4 step) -----

/// The edges of `graph` as `out-label->in` by name, sorted.
fn edge_names(graph: &TraversalGraph, label: &str) -> Vec<String> {
    let name = |v| {
        graph
            .vertex_properties(v)
            .unwrap()
            .and_then(|map| {
                map.iter()
                    .find(|(k, _)| k.as_str() == "name")
                    .map(|(_, n)| format!("{n:?}").trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "?".to_string())
    };
    let mut out: Vec<String> = graph
        .edges()
        .unwrap()
        .filter(|&e| graph.edge_label(e).unwrap() == Some(label))
        .filter_map(|e| graph.edge_endpoints(e).unwrap())
        .map(|(a, b)| format!("{}-{label}->{}", name(a), name(b)))
        .collect();
    out.sort();
    out
}

#[test]
fn merge_creates_once_and_later_rows_and_runs_match_it() {
    let q = "FOR x IN [1, 1, 2] MERGE (n:N {k: x}) RETURN n.k AS k";
    let (mut graph, rows) = after(q);
    assert_eq!(column(&rows, "k"), ["1", "1", "2"]);
    // W34: the second row matches the first row's creation.
    assert_eq!(count_label(&graph, "N"), 2);
    let before = dump(&graph);
    assert_eq!(column(&write(&mut graph, q), "k"), ["1", "1", "2"]);
    assert_eq!(dump(&graph), before, "a second run only matches");
    // Every match is a row (W31).
    let rows = write(&mut graph, "MERGE (p:person) RETURN p.name AS name");
    assert_eq!(rows.len(), 4);
}

#[test]
fn merge_between_bound_ends_matches_the_direction_written() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let q = |from: &str, to: &str| {
        format!(
            "MATCH (a:person {{name: '{from}'}}), (b:person {{name: '{to}'}}) \
             MERGE (a)-[r:knows]->(b) RETURN startNode(r).name AS s"
        )
    };
    assert_eq!(
        column(&write(&mut graph, &q("marko", "josh")), "s"),
        ["\"marko\""]
    );
    assert_eq!(count_edges(&graph, "knows"), 2);
    assert_eq!(
        column(&write(&mut graph, &q("josh", "marko")), "s"),
        ["\"josh\""]
    );
    assert_eq!(
        edge_names(&graph, "knows"),
        [
            "josh-knows->marko",
            "marko-knows->josh",
            "marko-knows->vadas"
        ]
    );
    // An incoming edge pattern, and a new node between two bound ends.
    write(
        &mut graph,
        "MATCH (a:person {name: 'vadas'}), (b:person {name: 'marko'}) MERGE (a)<-[:knows]-(b)",
    );
    assert_eq!(count_edges(&graph, "knows"), 3);
    let q = "MATCH (a:person {name: 'marko'}), (b:person {name: 'josh'}) \
             MERGE (a)-[:x]->(m:mid)<-[:y]-(b) RETURN m";
    write(&mut graph, q);
    write(&mut graph, q);
    assert_eq!(count_label(&graph, "mid"), 1);
    assert_eq!(edge_names(&graph, "y"), ["josh-y->?"]);
}

#[test]
fn merge_undirected_matches_either_direction_and_creates_left_to_right() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let q = |from: &str, to: &str| {
        format!(
            "MATCH (a:person {{name: '{from}'}}), (b:person {{name: '{to}'}}) \
             MERGE (a)-[r:knows]-(b) RETURN startNode(r).name AS s, endNode(r).name AS t"
        )
    };
    // marko-knows->josh exists: matched from either end (W33).
    let rows = write(&mut graph, &q("josh", "marko"));
    assert_eq!(column(&rows, "s"), ["\"marko\""]);
    assert_eq!(count_edges(&graph, "knows"), 2);
    // None between josh and vadas: created from the node written first.
    let rows = write(&mut graph, &q("josh", "vadas"));
    assert_eq!(column(&rows, "s"), ["\"josh\""]);
    assert_eq!(column(&rows, "t"), ["\"vadas\""]);
    let rows = write(&mut graph, &q("vadas", "josh"));
    assert_eq!(column(&rows, "s"), ["\"josh\""]);
    assert_eq!(count_edges(&graph, "knows"), 3);
    // Edges both ways: each one is a match, a self-loop matches once.
    write(
        &mut graph,
        "MATCH (a:person {name: 'vadas'}), (b:person {name: 'marko'}) INSERT (a)-[:knows]->(b)",
    );
    assert_eq!(write(&mut graph, &q("marko", "vadas")).len(), 2);
    let loop_q = "MATCH (a:person {name: 'peter'}) MERGE (a)-[r:self]-(a) RETURN r";
    write(&mut graph, loop_q);
    assert_eq!(write(&mut graph, loop_q).len(), 1);
    assert_eq!(count_edges(&graph, "self"), 1);
}

#[test]
fn merge_path_variable_reads_alike_created_and_matched() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let q = "MERGE p = (:x {k: 1})-[:t]->(:y) RETURN p";
    let created = write(&mut graph, q);
    let matched = write(&mut graph, q);
    assert_eq!(created, matched);
    // Node, edge, node.
    let path = |rows: &[ElementProperty]| column(rows, "p")[0].clone();
    let text = path(&created);
    assert!(
        text.starts_with('[') && text.matches(", ").count() == 2,
        "{text}"
    );
    assert!(text.contains("-t->"), "{text}");
    // From a bound end, also one a `WITH` column holds.
    let q = "MATCH (a:person {name: 'marko'}) WITH a, count(*) AS c \
             MERGE p = (a)-[:k]->(b:z) RETURN p";
    let created = write(&mut graph, q);
    assert_eq!(created, write(&mut graph, q));
    let text = path(&created);
    assert!(
        text.matches(", ").count() == 2 && text.contains("-k->"),
        "{text}"
    );
    assert_eq!(count_label(&graph, "z"), 1);
}

#[test]
fn merge_on_items_run_on_the_record_and_read_earlier_bindings() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let q = "MATCH (a:person {name: 'marko'}) MERGE (a)-[:likes]->(f:fruit {name: 'apple'}) \
             ON CREATE SET f.by = a.name ON MATCH SET f.again = true, a.fan = true \
             RETURN f.by AS by, f.again AS again";
    let rows = write(&mut graph, q);
    assert_eq!(column(&rows, "by"), ["\"marko\""]);
    assert_eq!(column(&rows, "again"), ["null"]);
    let rows = write(&mut graph, q);
    assert_eq!(column(&rows, "again"), ["true"]);
    let fans = write(
        &mut graph,
        "MATCH (a:person) WHERE a.fan RETURN a.name AS n",
    );
    assert_eq!(column(&fans, "n"), ["\"marko\""]);
    // A record column read after a later stage left the record (`#m<k>`).
    let rows = write(
        &mut graph,
        "MERGE (n:x {k: 1}) MATCH (m:person {name: 'josh'}) RETURN n.k AS k, m.name AS m",
    );
    assert_eq!(column(&rows, "k"), ["1"]);
    assert_eq!(column(&rows, "m"), ["\"josh\""]);
}

#[test]
fn merge_in_a_call_body_and_through_an_index_lookup() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let q = "MATCH (p:person) CALL (p) { MERGE (p)-[:likes]->(:fruit) } RETURN count(*) AS n";
    assert_eq!(column(&write(&mut graph, q), "n"), ["4"]);
    write(&mut graph, q);
    assert_eq!(count_edges(&graph, "likes"), 4);
    assert_eq!(count_label(&graph, "fruit"), 4);
    // With an index on `person.name` the match child is a lookup per row; same results.
    let q = "FOR x IN ['marko', 'zed', 'zed'] MERGE (n:person {name: x}) RETURN n.name AS n";
    let (plain, plain_rows) = after(q);
    let mut indexed = TraversalGraph::tinkerpop_modern();
    indexed
        .traversal_mut()
        .create_index(
            "person_name",
            graphersal::catalog::PropertyIndexDefinition::new(
                graphersal::storage::ElementKind::Vertex,
                "person",
                "name",
            ),
        )
        .unwrap();
    // The profile runs the query once: the same writes as without the index.
    let profile = indexed
        .traversal_mut()
        .gql(q)
        .profile()
        .unwrap_or_else(|e| panic!("{e}"))
        .to_string();
    assert!(profile.contains("gql:lookup("), "{profile}");
    assert_eq!(dump(&indexed), dump(&plain));
    let mut plain = plain;
    assert_eq!(write(&mut indexed, q), write(&mut plain, q));
    assert_eq!(column(&plain_rows, "n").len(), 3);
    assert_eq!(dump(&indexed), dump(&plain));
}

// ----- stage 3e: MERGE against its twins and in the corners (3e.T5) -----

/// The modern graph after `setup` (when not empty) and `query`; a failure panics.
fn state_after(setup: &str, query: &str) -> Vec<String> {
    let mut graph = TraversalGraph::tinkerpop_modern();
    if !setup.is_empty() {
        write(&mut graph, setup);
    }
    write(&mut graph, query);
    dump(&graph)
}

/// The GQL error of `query` on `graph`: its GQLSTATUS.
fn gql_status(graph: &mut TraversalGraph, query: &str) -> String {
    let err = graph
        .traversal_mut()
        .gql(query)
        .to_list()
        .expect_err("the query must fail");
    match err.root_cause() {
        TraverserError::Gql { gqlstatus, .. } => gqlstatus.to_string(),
        other => panic!("{query}: not a GQL error: {other:?}"),
    }
}

/// MERGE leaves the graph its pure-GQL twin leaves: per row, a unit `CALL` body that inserts
/// the pattern only when `NOT EXISTS` finds it (rows see the earlier rows' creations, W34).
/// Running the MERGE a second time changes nothing.
#[test]
fn merge_leaves_the_graph_its_conditional_insert_twin_leaves() {
    for (setup, merge, twin) in [
        (
            "",
            "FOR x IN [1, 1, 2] MERGE (:N {k: x})",
            "FOR x IN [1, 1, 2] CALL (x) { FILTER NOT EXISTS { MATCH (n:N {k: x}) } \
             INSERT (:N {k: x}) }",
        ),
        // Label sets are containment: an `:A` node is no `:A:B` node (W31).
        (
            "INSERT (:A {k: 1}), (:A&B&C {k: 2})",
            "FOR x IN [1, 2, 3] MERGE (:A:B {k: x})",
            "FOR x IN [1, 2, 3] CALL (x) { FILTER NOT EXISTS { MATCH (n:A&B {k: x}) } \
             INSERT (:A&B {k: x}) }",
        ),
        // GQL `=`: 1 = 1.0 matches.
        (
            "INSERT (:N {k: 1.0})",
            "FOR x IN [1, 2] MERGE (:N {k: x})",
            "FOR x IN [1, 2] CALL (x) { FILTER NOT EXISTS { MATCH (n:N {k: x}) } \
             INSERT (:N {k: x}) }",
        ),
        (
            "",
            "MATCH (a:person), (b:software) MERGE (a)-[:created]->(b)",
            "MATCH (a:person), (b:software) CALL (a, b) { \
             FILTER NOT EXISTS { MATCH (a)-[:created]->(b) } INSERT (a)-[:created]->(b) }",
        ),
        (
            "",
            "MATCH (p:person) MERGE (p)-[:likes]->(:fruit {name: p.name})",
            "MATCH (p:person) CALL (p) { \
             FILTER NOT EXISTS { MATCH (p)-[:likes]->(:fruit {name: p.name}) } \
             INSERT (p)-[:likes]->(:fruit {name: p.name}) }",
        ),
        // A new node between two bound ends; the rows repeat each pair twice.
        (
            "",
            "MATCH (a:person), (b:software) FOR i IN [1, 2] \
             MERGE (a)-[:x]->(:mid {by: a.name, of: b.name})<-[:y]-(b)",
            "MATCH (a:person), (b:software) FOR i IN [1, 2] CALL (a, b) { \
             FILTER NOT EXISTS { MATCH (a)-[:x]->(:mid {by: a.name, of: b.name})<-[:y]-(b) } \
             INSERT (a)-[:x]->(:mid {by: a.name, of: b.name})<-[:y]-(b) }",
        ),
        // A DELETE before it: the match does not see the deleted node.
        (
            "",
            "MATCH (s:software {name: 'lop'}) DETACH DELETE s FOR x IN ['lop', 'lop'] \
             MERGE (:software {name: x})",
            "MATCH (s:software {name: 'lop'}) DETACH DELETE s FOR x IN ['lop', 'lop'] \
             CALL (x) { FILTER NOT EXISTS { MATCH (n:software {name: x}) } \
             INSERT (:software {name: x}) }",
        ),
    ] {
        let merged = state_after(setup, merge);
        assert_eq!(merged, state_after(setup, twin), "{merge}\nvs {twin}");
        let mut graph = TraversalGraph::tinkerpop_modern();
        if !setup.is_empty() {
            write(&mut graph, setup);
        }
        write(&mut graph, merge);
        let once = dump(&graph);
        // The DELETE case deletes again on the second run: only the others are idempotent.
        if !merge.contains("DELETE") {
            write(&mut graph, merge);
            assert_eq!(dump(&graph), once, "{merge} twice");
        }
    }
}

#[test]
fn several_on_items_run_in_the_written_order() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let q = "MERGE (n:N {k: 1}) ON MATCH SET n.a = 1 ON CREATE SET n.c = 1 \
             ON CREATE SET n.c = n.c + 1, n.d = n.c ON MATCH SET n.a = n.a + 10, n:Seen \
             RETURN n.a AS a, n.c AS c, n.d AS d";
    let rows = write(&mut graph, q);
    assert_eq!(
        (column(&rows, "a"), column(&rows, "c"), column(&rows, "d")),
        (vec!["null".into()], vec!["2".into()], vec!["2".into()])
    );
    let rows = write(&mut graph, q);
    assert_eq!(
        (column(&rows, "a"), column(&rows, "c"), column(&rows, "d")),
        (vec!["11".into()], vec!["2".into()], vec!["2".into()])
    );
    assert_eq!(count_label(&graph, "Seen"), 1);
    // ON MATCH runs on every matched record, ON CREATE on none of them.
    let rows = write(
        &mut graph,
        "MERGE (p:person) ON CREATE SET p.new = true ON MATCH SET p.seen = p.name \
         RETURN p.seen AS s, p.new AS n",
    );
    assert_eq!(rows.len(), 4);
    assert!(column(&rows, "n").iter().all(|n| n == "null"), "{rows:?}");
    assert_eq!(
        write(&mut graph, "MATCH (p) WHERE p.seen = p.name RETURN p").len(),
        4
    );
}

/// A NULL property value (E1) and a NULL bound end fail the query with 22004 on the row that
/// meets them, after earlier rows created elements: the graph is left as it was.
#[test]
fn a_null_in_a_merge_fails_and_leaves_nothing() {
    let modern = dump(&TraversalGraph::tinkerpop_modern());
    for query in [
        "FOR x IN [1, 2, NULL] MERGE (:N {k: x})",
        "MATCH (p:person) MERGE (:N {k: p.name, l: p.age, m: p.lang})",
        "MATCH (p:person) MERGE (p)-[:r {w: p.age}]->(:N {k: p.name, l: p.lang})",
        "MATCH (p:person) MERGE (p)-[:r]->(:N {k: p.name}) ON CREATE SET p.x = 1 \
         WITH p MERGE (:M {k: p.lang})",
        // The bound end of the last rows is NULL: the edge has no end.
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) MERGE (p)-[:r]->(s)",
        "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) MERGE (s)<-[:r]-(:N {k: 1})",
    ] {
        let mut graph = TraversalGraph::tinkerpop_modern();
        assert_eq!(gql_status(&mut graph, query), "22004", "{query}");
        assert_eq!(dump(&graph), modern, "{query}");
    }
    // The message names the pattern map with its NULL.
    let err = TraversalGraph::tinkerpop_modern()
        .traversal_mut()
        .gql("FOR x IN [1, NULL] MERGE (:N {k: x})")
        .to_list()
        .unwrap_err();
    let text = format_diagnostic(&err);
    assert!(
        text.contains("MERGE cannot match or create `{k: NULL}`: a property value is NULL"),
        "{text}"
    );
}

/// A bound end held as a `WITH` column (the row no longer runs on the element): the edge, the
/// path and its value read alike to a later `MATCH p = ..`.
#[test]
fn merge_from_bound_ends_held_as_with_columns() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let matched = write(
        &mut graph,
        "MATCH (a:person {name: 'marko'}), (b:software {name: 'lop'}) WITH a, b, count(*) AS c \
         MERGE p = (a)-[:created]->(b) RETURN p",
    );
    let read = write(
        &mut graph,
        "MATCH p = (a:person {name: 'marko'})-[:created]->(b:software {name: 'lop'}) RETURN p",
    );
    assert_eq!(matched, read);
    assert_eq!(count_edges(&graph, "created"), 4);
    // Created between two columns, then matched, both directions written.
    let q = "MATCH (a:person {name: 'vadas'}), (b:software {name: 'ripple'}) \
             WITH b AS y, a AS x MERGE p = (y)<-[:used]-(x) RETURN p";
    let created = write(&mut graph, q);
    assert_eq!(created, write(&mut graph, q));
    assert_eq!(
        created,
        write(
            &mut graph,
            "MATCH p = (y {name: 'ripple'})<-[:used]-(x) RETURN p"
        )
    );
    assert_eq!(edge_names(&graph, "used"), ["vadas-used->ripple"]);
    // Both ends the same column: a self-loop.
    let q = "MATCH (a:person {name: 'peter'}) WITH a AS x, a AS y MERGE (x)-[:me]->(y)";
    write(&mut graph, q);
    write(&mut graph, q);
    assert_eq!(edge_names(&graph, "me"), ["peter-me->peter"]);
}

/// Several MERGE clauses in one query, a CALL body with one and a MATCH after them: every
/// record column (`#m<k>`, `#c<k>`) keeps its own name.
#[test]
fn several_merges_in_one_query_keep_their_columns_apart() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let q = "FOR x IN [1, 2, 1] MERGE (n:N {k: x}) MERGE (m:M {k: x * 10}) \
             MERGE (n)-[t:t]->(m) MERGE (m)-[u:u]->(n) \
             CALL (n) { MERGE (n)-[:v]->(w:W) RETURN w } \
             MATCH (p:person {name: 'marko'}) \
             RETURN n.k AS n, m.k AS m, type(t) AS t, type(u) AS u, labels(w) AS w, p.name AS p";
    let rows = write(&mut graph, q);
    assert_eq!(column(&rows, "n"), ["1", "2", "1"]);
    assert_eq!(column(&rows, "m"), ["10", "20", "10"]);
    assert_eq!(column(&rows, "t"), ["\"t\""; 3]);
    assert_eq!(column(&rows, "u"), ["\"u\""; 3]);
    assert_eq!(column(&rows, "w"), ["[\"W\"]"; 3]);
    assert_eq!(column(&rows, "p"), ["\"marko\""; 3]);
    let before = dump(&graph);
    assert_eq!(
        (
            count_label(&graph, "N"),
            count_label(&graph, "M"),
            count_label(&graph, "W")
        ),
        (2, 2, 2)
    );
    assert_eq!(
        (
            count_edges(&graph, "t"),
            count_edges(&graph, "u"),
            count_edges(&graph, "v")
        ),
        (2, 2, 2)
    );
    assert_eq!(write(&mut graph, q), rows);
    assert_eq!(dump(&graph), before);
}

/// On a larger graph the match child of a MERGE on an indexed property reads the index (the
/// profile names it); the rows and the graph are those of the scan without the index.
#[test]
fn an_indexed_merge_equals_the_scan_on_a_larger_graph() {
    let fixture = || {
        let mut graph = TraversalGraph::new();
        write(
            &mut graph,
            "FOR i IN range(1, 2000) INSERT (:person {name: 'p' + toString(i), i: i})",
        );
        graph
    };
    let q = "FOR x IN ['p7', 'zed', 'p1999', 'zed', 'p7'] MERGE (n:person {name: x}) \
             ON CREATE SET n.i = 0 ON MATCH SET n.hits = coalesce(n.hits, 0) + 1 \
             RETURN n.i AS i";
    let indexed_fixture = || {
        let mut graph = fixture();
        graph
            .traversal_mut()
            .create_index(
                "person_name",
                graphersal::catalog::PropertyIndexDefinition::new(
                    graphersal::storage::ElementKind::Vertex,
                    "person",
                    "name",
                ),
            )
            .unwrap();
        graph
    };
    let mut plain = fixture();
    let mut indexed = indexed_fixture();
    // The profile runs the query: on a graph of its own.
    let profile = indexed_fixture()
        .traversal_mut()
        .gql(q)
        .profile()
        .unwrap_or_else(|e| panic!("{e}"))
        .to_string();
    assert!(
        profile.contains("gql:lookup(") && profile.contains("[index: vertex person.name]"),
        "{profile}"
    );
    let rows = write(&mut indexed, q);
    assert_eq!(column(&rows, "i"), ["7", "0", "1999", "0", "7"]);
    assert_eq!(rows, write(&mut plain, q));
    assert_eq!(dump(&indexed), dump(&plain));
    assert_eq!(indexed.vertex_count(), 2001);
    assert_eq!(
        column(
            &write(
                &mut indexed,
                "MATCH (n:person) WHERE n.hits > 0 RETURN n.hits AS h ORDER BY h"
            ),
            "h"
        ),
        ["1", "1", "2"]
    );
}

// ----- MERGE without an index (scan lookup) -----

/// A MERGE whose pattern properties read the row starts its match child with a `gql:lookup`
/// that scans the label and compares the stored value directly (no index needed), and it
/// still sees the vertices earlier rows of the same query created.
#[test]
fn an_unindexed_merge_matches_through_a_scan_lookup_and_sees_earlier_rows() {
    let mut graph = TraversalGraph::new();
    let q = "UNWIND [1, 2, 1, 3, 2, 1] AS i MERGE (m:M {i: i}) \
             ON CREATE SET m.n = 1 ON MATCH SET m.n = m.n + 1";
    let profile = TraversalGraph::new()
        .traversal_mut()
        .gql(q)
        .profile()
        .unwrap_or_else(|e| panic!("{e}"))
        .to_string();
    assert!(
        profile.contains("gql:lookup(") && profile.contains("[scan: vertex M.i]"),
        "{profile}"
    );
    write(&mut graph, q);
    assert_eq!(graph.vertex_count(), 3);
    let rows = write(
        &mut graph,
        "MATCH (m:M) RETURN m.i AS i, m.n AS n ORDER BY i",
    );
    assert_eq!(column(&rows, "i"), ["1", "2", "3"]);
    assert_eq!(column(&rows, "n"), ["3", "2", "1"]);
}

/// The scan lookup leaves the graph the index-served and the filter-scanned forms leave, for
/// every kind of value (numbers across int and float, strings, booleans, NULL is an error).
#[test]
fn an_unindexed_merge_agrees_with_the_indexed_one_on_mixed_values() {
    let q = "UNWIND [1, 1.0, 'a', 'a', true, false, true, 2] AS v MERGE (:M {k: v})";
    let mut plain = TraversalGraph::new();
    write(&mut plain, q);
    let mut indexed = TraversalGraph::new();
    indexed
        .traversal_mut()
        .create_index(
            "m_k",
            graphersal::catalog::PropertyIndexDefinition::new(
                graphersal::storage::ElementKind::Vertex,
                "M",
                "k",
            ),
        )
        .unwrap();
    write(&mut indexed, q);
    assert_eq!(dump(&plain), dump(&indexed));
}

/// A failing query leaves nothing of the merges before the failure (rollback).
#[test]
fn an_unindexed_merge_rolls_back_with_its_query() {
    let mut graph = TraversalGraph::new();
    let result = graph
        .traversal_mut()
        .gql("UNWIND [1, 2, NULL] AS i MERGE (:M {i: i})")
        .to_list();
    assert!(result.is_err());
    assert_eq!(graph.vertex_count(), 0);
}

/// The shape of the cost: 6000 distinct keys merge in well under the time a per-candidate
/// evaluator needs (that took about 2 s here, the scan lookup about a tenth of it); the
/// bound is loose so a loaded machine does not fail it, but a regression to the evaluator
/// plus a debug build would.
#[test]
fn an_unindexed_merge_batch_is_not_dominated_by_the_evaluator() {
    let mut graph = TraversalGraph::new();
    let started = std::time::Instant::now();
    write(&mut graph, "UNWIND range(1, 6000) AS i MERGE (:M {i: i})");
    write(&mut graph, "UNWIND range(1, 6000) AS i MERGE (:M {i: i})");
    assert_eq!(graph.vertex_count(), 6000);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
}
