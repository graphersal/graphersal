//! GQL stage 2b `EXISTS` and pattern predicates, the fast path (feature `gql`): a top-level
//! `[NOT] EXISTS` conjunct of `WHERE` / `FILTER` / an inline `WHERE` (a Cypher pattern
//! predicate too) lowers to `filter(__.<body>)` / `not(__.<body>)`, the body planned with its
//! imports as anchors (design M2, E7, E11). Differential twins against Gremlin `where(__..)` /
//! `not(__..)` (Rhai) compare the rows as multisets (design D18); `NULL` imports (E9) and trail
//! inside a body (E8) are hand-computed rows. A subquery anywhere else in an expression (under
//! `OR` / `CASE`, in `RETURN`, `ORDER BY`, an aggregate's argument, a `FOR` list) runs as a
//! child of the `gql:*` step evaluating it (stage 2b.T5, the hook): hand-computed rows,
//! three-valued logic around it, the profile children, and every walker visiting them.
//! Stage 2d.T5c: `COUNT { }` of `MATCH` clauses on the hook (twins against `local(__..count())`
//! and `OPTIONAL MATCH .. count()`), and the correlated `gql:lookup` starting a body's first
//! component.
#![cfg(feature = "gql")]

use std::sync::Arc;

use graphersal::prelude::*;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

fn rows(graph: &Arc<Graph>, query: &str) -> Vec<ElementProperty> {
    graph
        .read()
        .traversal()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}

fn error(graph: &Arc<Graph>, query: &str) -> String {
    match graph.read().traversal().gql(query).to_list() {
        Ok(rows) => panic!("{query}: expected an error, got {rows:?}"),
        Err(e) => e.to_string(),
    }
}

fn profile(graph: &Arc<Graph>, query: &str) -> String {
    graph
        .read()
        .traversal()
        .gql(query)
        .profile()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .to_string()
}

/// The steps of a profile, without the metrics columns (nested steps keep their `\>` marker).
fn steps(profile: &str) -> Vec<String> {
    profile
        .lines()
        .skip_while(|line| !line.starts_with("====="))
        .skip(1)
        .take_while(|line| !line.starts_with("====="))
        .filter(|line| !line.trim_start().starts_with('[') && !line.contains("TOTAL:"))
        .map(|line| {
            let step = line.trim_start();
            step[..step.find("  ").unwrap_or(step.len())].to_string()
        })
        .collect()
}

/// Whether a profile holds an `as()` step (top level or in a child).
fn labelled(profile: &str) -> bool {
    steps(profile)
        .iter()
        .any(|s| s.starts_with("as(") || s.starts_with("\\> as(") || s.contains(".as("))
}

fn norm(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => format!("'{s}'"),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => match n.as_i64() {
            Some(i) if n.is_i64() => format!("i:{i}"),
            _ => format!("{n:?}"),
        },
        ElementProperty::Object(map) => {
            let mut items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", norm(v)))
                .collect();
            items.sort();
            format!("{{{}}}", items.join(", "))
        }
        ElementProperty::Vertex(v) => format!("v{}", v.id()),
        ElementProperty::Edge(e) => format!("e{}", e.id()),
        other => format!("{other:?}"),
    }
}

/// The `n` column of every row, sorted (a multiset).
fn names(graph: &Arc<Graph>, query: &str) -> Vec<String> {
    let mut out: Vec<String> = rows(graph, query)
        .iter()
        .map(|row| match row {
            ElementProperty::Object(map) => {
                norm(map.get("n").unwrap_or_else(|| panic!("{query}: no n")))
            }
            other => panic!("{query}: not a row map: {other:?}"),
        })
        .collect();
    out.sort();
    out
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    out.sort();
    out
}

// ----- differential twins against Gremlin (Rhai) -----

#[cfg(feature = "script")]
mod twins {
    use std::sync::Arc;

    use graphersal::prelude::*;

    use super::{modern, norm};

    /// The values of the Rhai script `script` (a traversal), as sorted `norm` texts.
    fn values(graph: &Arc<Graph>, script: &str) -> Vec<String> {
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
        let lock = graph.read();
        let mut out: Vec<String> = gts
            .ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .iter()
            .map(norm)
            .collect();
        out.sort();
        out
    }

    /// The `x` column of the GQL query `gql` against the values of the Gremlin twin, as
    /// multisets.
    fn twin(graph: &Arc<Graph>, gql: &str, gremlin: &str) {
        let left = values(graph, &format!("g.gql({gql:?}).select(\"x\")"));
        let right = values(graph, gremlin);
        assert!(!left.is_empty(), "{gql}: no rows to compare");
        assert_eq!(left, right, "\nGQL:  {gql}\ntwin: {gremlin}");
    }

    #[test]
    fn exists_matches_where_and_not_exists_matches_not() {
        let graph = modern();
        for (gql, gremlin) in [
            // "Who has no ...": the anti-join.
            (
                "MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name AS x",
                r#"g.V().hasLabel("person").not(__.out("created")).values("name")"#,
            ),
            (
                "MATCH (p:person) WHERE NOT EXISTS { MATCH (p)-[:created]->() } RETURN p.name AS x",
                r#"g.V().hasLabel("person").not(__.out("created")).values("name")"#,
            ),
            // The semi-join never multiplies rows (josh created two things: one row).
            (
                "MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.out("created")).values("name")"#,
            ),
            (
                "MATCH (p:person) WHERE (p)-[:created]->() RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.out("created")).values("name")"#,
            ),
            (
                "MATCH (p:person) WHERE EXISTS ( (p)<-[:knows]-() ) RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.in("knows")).values("name")"#,
            ),
            // A condition inside the body.
            (
                "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WHERE s.lang = 'java' AND s.name = 'ripple' } RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.out("created").has("lang", "java").has("name", "ripple")).values("name")"#,
            ),
            // Correlation with two imports: the join is `where(P.eq("a"))`.
            (
                "MATCH (a:person), (b:person) WHERE (a)-[:knows]->(b) RETURN b.name AS x",
                r#"g.V().hasLabel("person").as("a").V().hasLabel("person").where(__.in("knows").where(P.eq("a"))).values("name")"#,
            ),
            (
                "MATCH (a:person), (b:person) WHERE NOT (a)-[:knows]->(b) RETURN b.name AS x",
                r#"g.V().hasLabel("person").as("a").V().hasLabel("person").not(__.in("knows").where(P.eq("a"))).values("name")"#,
            ),
            // On a node reached by a hop, and in an inline WHERE.
            (
                "MATCH (p:person)-[:knows]->(f) WHERE EXISTS { (f)-[:created]->() } RETURN f.name AS x",
                r#"g.V().hasLabel("person").out("knows").where(__.out("created")).values("name")"#,
            ),
            (
                "MATCH (p:person WHERE NOT EXISTS { (p)-[:knows]->() })-[:created]->(s) RETURN s.name AS x",
                r#"g.V().hasLabel("person").not(__.out("knows")).out("created").values("name")"#,
            ),
            // A quantified edge inside the body.
            (
                "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:knows]->{1,2}(f) WHERE f.name = 'josh' } RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.repeat(__.out("knows")).emit().times(2).has("name", "josh")).values("name")"#,
            ),
            // Nested EXISTS.
            (
                "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WHERE EXISTS { (s)<-[:created]-(o) WHERE o.age > 33 } } RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.out("created").where(__.in("created").has("age", P.gt(33)))).values("name")"#,
            ),
            // A body that imports nothing: a child-leading scan.
            (
                "MATCH (p:person) WHERE EXISTS { MATCH (s:software {lang: 'java'}) } RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.V().has("software", "lang", "java")).values("name")"#,
            ),
            // An import from a materializing WITH column (`gql:bind` in the child).
            (
                "MATCH (p:person) WITH p, count(*) AS c WHERE EXISTS { (p)-[:knows]->() } RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.out("knows")).values("name")"#,
            ),
            // FILTER after a FOR.
            (
                "FOR i IN [1, 2] MATCH (p:person) FILTER EXISTS { (p)-[:created]->(:software {name: 'lop'}) } RETURN p.name AS x",
                r#"g.inject(1, 2).V().hasLabel("person").where(__.out("created").has("name", "lop")).values("name")"#,
            ),
        ] {
            twin(&graph, gql, gremlin);
        }
    }
}

// ----- plans -----

#[test]
fn who_has_no_lowers_to_not_without_a_label() {
    let graph = modern();
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name AS n",
    );
    assert_eq!(
        steps(&text),
        [
            "v(labels: [\"person\"])",
            "not(__.out(\"created\"))",
            "\\> out(\"created\")",
            "gql:project(\"n\")"
        ],
        "{text}"
    );
    // The EXISTS { MATCH .. } spelling lowers the same.
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE NOT EXISTS { MATCH (p)-[:created]->() } RETURN p.name AS n",
    );
    assert!(text.contains("not(__.out(\"created\"))"), "{text}");
    assert!(!labelled(&text), "{text}");
    // EXISTS is `filter(__..)`; a body local read only on itself gets no label. The lowering
    // places no merge point of its own inside a child: the one between the hops is the global
    // `LazyBarrierRule`'s, exactly as in the Gremlin twin `where(__.out("knows").out(..))`.
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:knows]->(f)-[:created]->(s) WHERE s.lang = 'java' } RETURN p.name AS n",
    );
    assert_eq!(
        steps(&text),
        [
            "v(labels: [\"person\"])",
            "filter(__.out(\"knows\").lazy_barrier().out(\"created\").has(\"lang\", P.eq(\"java\")))",
            "\\> out(\"knows\")",
            "\\> lazy_barrier()",
            "\\> out(\"created\")",
            "\\> has(\"lang\", P.eq(\"java\"))",
            "gql:project(\"n\")"
        ],
        "{text}"
    );
    assert!(!labelled(&text), "{text}");
    let twin = graph
        .read()
        .traversal()
        .v(None)
        .has_label("person")
        .where_t(__::out("knows").out("created"))
        .profile()
        .unwrap()
        .to_string();
    assert!(
        twin.contains("where(__.out(\"knows\").lazy_barrier().out(\"created\"))"),
        "{twin}"
    );
}

#[test]
fn exists_runs_last_on_its_element() {
    let graph = modern();
    // Design E11: the lowering emits pushed has(), then gql:filter residuals, then the EXISTS
    // child; the global `FilterReorderRule` may then move filters among themselves by cost (as
    // in any Gremlin plan), never before the pushed has().
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE NOT (p)-->() AND p.age > 20 AND p.name <> 'x' RETURN p.name AS n",
    );
    let steps = steps(&text);
    let at = |prefix: &str| {
        steps
            .iter()
            .position(|s| s.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in {text}"))
    };
    assert!(at("has(\"age\"") < at("gql:filter"), "{text}");
    assert!(at("has(\"age\"") < at("not("), "{text}");
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE NOT (p)-->() AND p.age > 20 AND p.name <> 'x' RETURN p.name AS n"
        ),
        sorted(&["vadas"])
    );
}

#[test]
fn imports_read_away_from_the_current_element_are_labelled() {
    let graph = modern();
    // A correlation with a second import: that one is labelled and joined.
    let text = profile(
        &graph,
        "MATCH (a:person), (b:person) WHERE (a)-[:knows]->(b) RETURN b.name AS n",
    );
    assert!(text.contains("as(\"a\")"), "{text}");
    assert!(!text.contains("as(\"b\")"), "{text}");
    assert!(
        text.contains("filter(__.in(\"knows\").where(P.eq(\"a\")))"),
        "{text}"
    );
    // A body condition reading the import after the body moved on labels it, before the child.
    let text = profile(
        &graph,
        "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:knows]->(f) WHERE f.age > p.age } RETURN p.name AS n",
    );
    let steps = steps(&text);
    let label = steps.iter().position(|s| s.starts_with("as(\"p\")"));
    let child = steps.iter().position(|s| s.starts_with("filter("));
    assert!(label.is_some() && label < child, "{text}");
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:knows]->(f) WHERE f.age > p.age } RETURN p.name AS n"
        ),
        sorted(&["marko"])
    );
}

// ----- semantics: NULL imports (E9), trail (E8), path variables, bulk -----

#[test]
fn a_null_import_makes_exists_false() {
    let graph = modern();
    // Only josh created ripple; for everyone else `s` is NULL: EXISTS FALSE, NOT EXISTS TRUE.
    let base = "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s {name: 'ripple'})";
    assert_eq!(
        names(
            &graph,
            &format!("{base} FILTER EXISTS {{ (s)<-[:created]-() }} RETURN p.name AS n")
        ),
        sorted(&["josh"])
    );
    assert_eq!(
        names(
            &graph,
            &format!("{base} FILTER NOT EXISTS {{ (s)<-[:created]-() }} RETURN p.name AS n")
        ),
        sorted(&["marko", "vadas", "peter"])
    );
    // A NULL import read in a body condition: no match either.
    assert_eq!(
        names(
            &graph,
            &format!(
                "{base} FILTER NOT EXISTS {{ MATCH (x:software) WHERE x.name = s.name }} \
                 RETURN p.name AS n"
            )
        ),
        sorted(&["marko", "vadas", "peter"])
    );
}

#[test]
fn trail_holds_within_each_body_clause_and_outer_edges_may_repeat() {
    let graph = modern();
    // Josh alone created ripple: a second, different `created` edge into it does not exist.
    assert!(
        rows(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s {name: 'ripple'})<-[:created]-(o) } RETURN p.name AS n"
        )
        .is_empty()
    );
    // Two body clauses: trail per clause, so the same edge may be walked in each.
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s {name: 'ripple'}) MATCH (s)<-[:created]-(o) } RETURN p.name AS n"
        ),
        sorted(&["josh"])
    );
    // An edge bound outside is bound again inside (design E8).
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person)-[e:created]->(s {name: 'ripple'}) WHERE EXISTS { MATCH (p)-[e]->(s) } RETURN p.name AS n"
        ),
        sorted(&["josh"])
    );
    // Against an outer edge in a body condition.
    assert_eq!(
        names(
            &graph,
            "MATCH (a)-[e]->(b) WHERE NOT EXISTS { MATCH (a)-[e2]->(c)<-[e3]-(d) WHERE e2 <> e } RETURN a.name AS n"
        ),
        sorted(&["marko", "josh", "peter"])
    );
}

#[test]
fn path_variables_and_edges_inside_a_body() {
    let graph = modern();
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH x = (p)-[:created]->(s) WHERE length(x) = 1 AND s.lang = 'java' } RETURN p.name AS n"
        ),
        sorted(&["marko", "josh", "peter"])
    );
    // On an edge (an inline WHERE of the edge), importing the edge.
    let mut found: Vec<String> = rows(
        &graph,
        "MATCH (a)-[e WHERE NOT EXISTS { MATCH ()-[e]->(:software) }]->(b) RETURN a.name AS n, b.name AS m",
    )
    .iter()
    .map(norm)
    .collect();
    found.sort();
    assert_eq!(
        found,
        [
            "{m: 'josh', n: 'marko'}".to_string(),
            "{m: 'vadas', n: 'marko'}".to_string()
        ]
    );
}

#[test]
fn let_and_pattern_predicates_substitute_into_fast_path_conjuncts() {
    let graph = modern();
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) LET x = EXISTS { (p)-[:created]->() } FILTER x RETURN p.name AS n"
        ),
        sorted(&["marko", "josh", "peter"])
    );
    assert!(
        rows(
            &graph,
            "MATCH (p:person) LET x = EXISTS { (p)-[:created]->() } FILTER x AND NOT x RETURN \
             p.name AS n"
        )
        .is_empty()
    );
}

#[test]
fn merged_rows_test_like_unmerged_ones() {
    let graph = modern();
    let graph = graph.read();
    for query in [
        "MATCH (:person)-[:created]->(s) WHERE EXISTS { (s)<-[:created]-(:person {name: 'peter'}) } RETURN s.name AS n, count(*) AS c",
        "MATCH (:person)-[:knows]->(f) WHERE NOT (f)-[:created]->(:software {name: 'ripple'}) RETURN f.name AS n, count(*) AS c",
        "MATCH (a)-->(b)-->(c) WHERE EXISTS { (c)<-[:created]-() } RETURN count(*) AS c",
    ] {
        let run = |merge: bool| {
            let mut out: Vec<String> = graph
                .traversal()
                .with("bulk.merge", merge)
                .gql(query)
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {e}"))
                .iter()
                .map(norm)
                .collect();
            out.sort();
            out
        };
        assert_eq!(run(true), run(false), "{query}");
    }
    let rows = graph
        .traversal()
        .gql("MATCH (:person)-[:created]->(s) WHERE EXISTS { (s)<-[:created]-(:person {name: 'peter'}) } RETURN s.name AS n, count(*) AS c")
        .to_list()
        .unwrap();
    assert_eq!(
        rows.iter().map(norm).collect::<Vec<_>>(),
        ["{c: i:3, n: 'lop'}"]
    );
}

// ----- limits -----

#[test]
fn too_many_lowered_bodies_are_a_limit() {
    let graph = modern();
    let a = vec!["a"; 40].join(" AND ");
    let b = vec!["b"; 40].join(" AND ");
    let query = format!(
        "MATCH (p) LET a = EXISTS {{ (p)-->() }} LET b = EXISTS {{ MATCH (p)-->(q) WHERE {a} }} \
         FILTER {b} RETURN p.name AS n"
    );
    let err = error(&graph, &query);
    assert!(err.contains("54001"), "{err}");
    assert!(err.contains("subquery bodies"), "{err}");
    // Below the bound the same shape runs.
    let b = vec!["b"; 20].join(" AND ");
    let query = format!(
        "MATCH (p) LET a = EXISTS {{ (p)-->() }} LET b = EXISTS {{ MATCH (p)-->(q) WHERE {a} }} \
         FILTER {b} RETURN p.name AS n"
    );
    assert_eq!(names(&graph, &query), sorted(&["marko", "josh", "peter"]));
}

#[test]
fn the_deepest_nesting_runs_and_profiles() {
    let graph = modern();
    let mut deepest = None;
    let mut nested = "(n)-->()".to_string();
    for _ in 0..200 {
        nested = format!("EXISTS {{ MATCH (n) WHERE {nested} }}");
        let query = format!("MATCH (n) WHERE {nested} RETURN n.name AS n");
        match graph.read().traversal().gql(&query).to_list() {
            Ok(_) => deepest = Some(query),
            Err(e) if e.to_string().contains("42000") || e.to_string().contains("54001") => {
                break;
            }
            Err(e) => panic!("{query}: {e}"),
        }
    }
    let query = deepest.unwrap_or_else(|| panic!("nothing ran"));
    assert!(query.matches("EXISTS").count() > 10, "{query}");
    assert_eq!(names(&graph, &query), sorted(&["marko", "josh", "peter"]));
    assert!(profile(&graph, &query).contains("filter("));
}

// ----- the hook: EXISTS anywhere in an expression (stage 2b.T5) -----

/// The columns of every row as `name=value` texts in written key order, rows sorted (a
/// multiset) unless `ordered`.
fn table(graph: &Arc<Graph>, query: &str, ordered: bool) -> Vec<String> {
    let mut out: Vec<String> = rows(graph, query)
        .iter()
        .map(|row| match row {
            ElementProperty::Object(map) => map
                .iter()
                .map(|(k, v)| format!("{k}={}", norm(v)))
                .collect::<Vec<_>>()
                .join(" "),
            other => panic!("{query}: not a row map: {other:?}"),
        })
        .collect();
    if !ordered {
        out.sort();
    }
    out
}

fn strings(items: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    out.sort();
    out
}

#[test]
fn exists_runs_under_or_case_and_inside_a_body() {
    let graph = modern();
    // Out-edges in `modern`: marko knows vadas and josh and created lop; josh created ripple
    // and lop; peter created lop; vadas has none.
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE p.age > 30 OR EXISTS { (p)-->() } RETURN p.name AS n"
        ),
        sorted(&["marko", "josh", "peter"])
    );
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE p.age < 28 OR (p)-[:knows]->() RETURN p.name AS n"
        ),
        sorted(&["marko", "vadas"])
    );
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE CASE WHEN (p)-[:created]->(:software {name: 'ripple'}) \
             THEN true ELSE p.age < 28 END RETURN p.name AS n"
        ),
        sorted(&["vadas", "josh"])
    );
    // `OR` inside a body: q has an age or out-edges of its own.
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(q) WHERE q.age > 1 OR (q)-->() } \
             RETURN p.name AS n"
        ),
        sorted(&["marko"])
    );
    // A nested hook inside a hosted body.
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE p.age > 100 OR EXISTS { MATCH (p)-[:created]->(s) \
             WHERE s.lang = 'none' OR EXISTS { (s)<-[:created]-(:person {name: 'peter'}) } } \
             RETURN p.name AS n"
        ),
        sorted(&["marko", "josh", "peter"])
    );
}

#[test]
fn exists_runs_in_return_order_by_aggregates_and_for_lists() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, EXISTS { (p)-->() } AS x, \
             CASE WHEN (p)-[:knows]->() THEN 1 END AS k",
            false
        ),
        strings(&[
            "n='marko' x=true k=i:1",
            "n='vadas' x=false k=null",
            "n='josh' x=true k=null",
            "n='peter' x=true k=null",
        ])
    );
    // Uncorrelated: a unit row.
    assert_eq!(
        table(
            &graph,
            "RETURN EXISTS { MATCH (s:software) WHERE s.lang = 'java' } AS a, \
             EXISTS { MATCH (s:software) WHERE s.lang = 'cobol' } AS b",
            false
        ),
        strings(&["a=true b=false"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n \
             ORDER BY EXISTS { (p)-[:created]->(:software {name: 'lop'}) } DESC, n",
            true
        ),
        vec!["n='josh'", "n='marko'", "n='peter'", "n='vadas'"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n ORDER BY EXISTS { (p)-[:knows]->() } DESC, p.age",
            true
        ),
        vec!["n='marko'", "n='vadas'", "n='josh'", "n='peter'"]
    );
    // An aggregate's argument and a grouping key.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN count(CASE WHEN (p)-[:created]->() THEN 1 END) AS c, \
             collect_list(EXISTS { (p)-[:knows]->() }) AS l",
            false
        ),
        vec!["c=i:3 l=[true, false, false, false]"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN EXISTS { (p)-[:knows]->() } AS k, count(*) AS c",
            false
        ),
        strings(&["k=true c=i:1", "k=false c=i:3"])
    );
    // A FOR list.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) FOR x IN [EXISTS { (p)-[:created]->() }, EXISTS { (p)-[:knows]->() }] \
             RETURN p.name AS n, x",
            false
        ),
        strings(&[
            "n='marko' x=true",
            "n='marko' x=true",
            "n='vadas' x=false",
            "n='vadas' x=false",
            "n='josh' x=true",
            "n='josh' x=false",
            "n='peter' x=true",
            "n='peter' x=false",
        ])
    );
}

#[test]
fn exists_in_the_order_by_of_an_aggregation_imports_the_columns() {
    // After `gql:aggregate` (a fresh path) the body reads an import a column holds from the
    // row: `gql:bind` instead of `select()`, a join to it a row check, a read inside the body
    // through the row label.
    let graph = modern();
    for (query, expected) in [
        (
            "MATCH (p:person) WITH p AS q, count(*) AS c \
             ORDER BY EXISTS { (p)-[:created]->() }, q.name RETURN q.name AS n",
            ["vadas", "josh", "marko", "peter"],
        ),
        (
            "MATCH (p:person) WITH p, count(*) AS c \
             ORDER BY EXISTS { (p)-[:knows]->(f) WHERE f.age > p.age }, p.name \
             RETURN p.name AS n",
            ["josh", "peter", "vadas", "marko"],
        ),
        (
            "MATCH (p:person) WITH p, count(*) AS c \
             ORDER BY EXISTS { (:software {name: 'ripple'})<-[:created]-(p) } DESC, p.name \
             RETURN p.name AS n",
            ["josh", "marko", "peter", "vadas"],
        ),
        (
            "MATCH (p:person) WITH p, count(*) AS c \
             ORDER BY EXISTS { (p)-[:knows]->(f) \
               WHERE EXISTS { (f)-[:created]->(s) WHERE p.age < 30 } } DESC, p.name \
             RETURN p.name AS n",
            ["marko", "josh", "peter", "vadas"],
        ),
        (
            "MATCH (p:person) RETURN p.name AS n, p, count(*) AS c \
             ORDER BY EXISTS { (p)-[:created]->() }, p.name DESC",
            ["vadas", "peter", "marko", "josh"],
        ),
        (
            "MATCH (p:person) WITH DISTINCT p \
             ORDER BY EXISTS { (p)-[:created]->() }, p.name RETURN p.name AS n",
            ["vadas", "josh", "marko", "peter"],
        ),
    ] {
        let rows = table(&graph, query, true);
        let expected: Vec<String> = expected.iter().map(|n| format!("n='{n}'")).collect();
        let names: Vec<String> = rows
            .iter()
            .map(|row| {
                row.split(' ')
                    .find(|cell| cell.starts_with("n="))
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(names, expected, "{query}");
    }
    // An edge import joined inside the body.
    assert_eq!(
        table(
            &graph,
            "MATCH (:person)-[e:knows]->() WITH e, count(*) AS c \
             ORDER BY EXISTS { (:person)-[e]->(:person {name: 'josh'}) } DESC \
             RETURN e.weight AS w",
            true
        ),
        vec!["w=1", "w=0.5"]
    );
}

#[test]
fn exists_composes_three_valued_and_a_null_import_is_false() {
    let graph = modern();
    // f is NULL for vadas, josh and peter (no knows edge): EXISTS over it is FALSE (E9), never
    // NULL, so NULL OR FALSE is NULL and NULL AND NOT EXISTS is NULL AND TRUE, NULL.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) OPTIONAL MATCH (p)-[:knows]->(f) RETURN p.name AS n, \
             EXISTS { (f)-->() } AS e, NOT EXISTS { (f)-->() } AS ne, \
             f.age > 28 OR EXISTS { (f)-->() } AS o, f.age > 28 AND NOT EXISTS { (f)-->() } AS a, \
             EXISTS { (f)-->() } IS NULL AS z",
            false
        ),
        strings(&[
            "n='marko' e=false ne=true o=false a=false z=false",
            "n='marko' e=true ne=false o=true a=false z=false",
            "n='vadas' e=false ne=true o=null a=null z=false",
            "n='josh' e=false ne=true o=null a=null z=false",
            "n='peter' e=false ne=true o=null a=null z=false",
        ])
    );
    // TRUE OR .. and FALSE AND .. decide without the subquery; UNKNOWN OR TRUE is TRUE.
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE p.nothing > 1 OR EXISTS { (p)-[:knows]->() } \
             RETURN p.name AS n"
        ),
        sorted(&["marko"])
    );
    // XOR with EXISTS.
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE p.age > 30 XOR (p)-[:created]->() RETURN p.name AS n"
        ),
        sorted(&["marko"])
    );
}

#[test]
fn a_subquery_runs_only_when_the_evaluation_reaches_it_and_obeys_the_limits() {
    let graph = modern();
    let lock = graph.read();
    let body = "EXISTS { MATCH (p)-[:knows]->*(q) WHERE q.age > 100 }";
    // `p.age > 0` decides every row: the unbounded quantifier never runs, so the loop limit is
    // never reached.
    let rows = lock
        .traversal()
        .with("repeat.max_loops", 1)
        .gql(&format!(
            "MATCH (p:person) WHERE p.age > 0 OR {body} RETURN p.name AS n"
        ))
        .to_list()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows.len(), 4);
    // Reached, the child obeys repeat.max_loops like any traversal (D14: an error, never a cut
    // result), in every host.
    for query in [
        format!("MATCH (p:person) WHERE p.age > 100 OR {body} RETURN p.name AS n"),
        format!("MATCH (p:person) RETURN {body} AS x"),
        format!("MATCH (p:person) RETURN p.name AS n ORDER BY {body}"),
        format!("MATCH (p:person) RETURN count(CASE WHEN {body} THEN 1 END) AS c"),
        format!("MATCH (p:person) FOR x IN [{body}] RETURN x"),
    ] {
        let error = lock
            .traversal()
            .with("repeat.max_loops", 1)
            .gql(&query)
            .to_list()
            .expect_err(&query);
        assert!(
            matches!(
                error.root_cause(),
                TraverserError::RepeatLimitExceeded { max_loops: 1, .. }
            ),
            "{query}: {error}"
        );
    }
}

#[test]
fn each_host_shows_its_children_and_the_walkers_visit_them() {
    let graph = modern();
    // Every host step: the profile lists its child's steps under it (`\>`), numbered like the
    // children of a Gremlin step; the optimizer folded the child's label into its source
    // (`v(labels: ["software"])`).
    let uncorrelated = "EXISTS { MATCH (s:software) WHERE s.lang = 'java' }";
    for (host, query) in [
        (
            "gql:filter(",
            format!("MATCH (p:person) WHERE p.age > 100 OR {uncorrelated} RETURN p.name AS n"),
        ),
        (
            "gql:project(",
            format!("MATCH (p:person) RETURN {uncorrelated} AS x"),
        ),
        (
            "gql:order(",
            format!("MATCH (p:person) RETURN p.name AS n ORDER BY {uncorrelated}"),
        ),
        (
            "gql:aggregate(",
            format!("MATCH (p:person) RETURN count(CASE WHEN {uncorrelated} THEN 1 END) AS c"),
        ),
        (
            "gql:for(",
            format!("MATCH (p:person) FOR x IN [{uncorrelated}] RETURN x"),
        ),
    ] {
        let steps = steps(&profile(&graph, &query));
        let at = steps
            .iter()
            .position(|s| s.starts_with(host))
            .unwrap_or_else(|| panic!("{query}: no {host} in {steps:?}"));
        assert_eq!(
            steps.get(at + 1).map(String::as_str),
            Some("\\> v(labels: [\"software\"])"),
            "{query}: {steps:?}"
        );
    }
    // The path analysis keeps the label an import is read through inside a child (`p` read
    // after the walk moved to `f`), and only then.
    let query =
        "MATCH (p:person)-[:knows]->(f) RETURN f.name AS n, EXISTS { (p)-[:created]->() } AS c";
    assert!(labelled(&profile(&graph, query)), "{query}");
    assert_eq!(
        table(&graph, query, false),
        strings(&["n='vadas' c=true", "n='josh' c=true"])
    );
    let query = "MATCH (p:person) RETURN p.name AS n, EXISTS { (p)-[:created]->() } AS c";
    assert!(!labelled(&profile(&graph, query)), "{query}");
}

/// Stage 2b.T6 (found by the 2b fuzz family): a condition holding a subquery that the hook
/// answers (`gql:filter` on an element) runs after the element's `as()`, like a fast-path
/// child, so a body reading the element through its label (a second body clause resuming at
/// it, a path variable for it) finds it: the hook and the fast path give the same rows.
#[test]
fn a_hooked_condition_on_an_element_reads_it_through_its_label() {
    let graph = modern();
    for (fast, hooked, expected) in [
        (
            // Second body clause resumes at `a` after the first moved away.
            "MATCH (a:person) WHERE EXISTS { MATCH (a)<-[:knows]-() MATCH (a)--() } \
             RETURN a.name AS n",
            "MATCH (a:person) WHERE CASE WHEN EXISTS { MATCH (a)<-[:knows]-() MATCH (a)--() } \
             THEN true ELSE false END RETURN a.name AS n",
            &["vadas", "josh"][..],
        ),
        (
            // A path variable over the imported element.
            "MATCH (a:person) WHERE EXISTS { MATCH q = (a)-[:created]->() WHERE length(q) = 1 } \
             RETURN a.name AS n",
            "MATCH (a:person) WHERE EXISTS { MATCH q = (a)-[:created]->() WHERE length(q) = 1 } \
             OR a.age > 100 RETURN a.name AS n",
            &["marko", "josh", "peter"][..],
        ),
        (
            // On an expansion target, under NOT and OR.
            "MATCH (p:person)-[:knows]->(b) WHERE NOT EXISTS { MATCH (b)<-[:knows]-() MATCH \
             (b)-[:created]->() } RETURN b.name AS n",
            "MATCH (p:person)-[:knows]->(b) WHERE NOT EXISTS { MATCH (b)<-[:knows]-() MATCH \
             (b)-[:created]->() } OR b.age > 100 RETURN b.name AS n",
            &["vadas"][..],
        ),
    ] {
        assert_eq!(names(&graph, fast), sorted(expected), "{fast}");
        assert_eq!(names(&graph, hooked), sorted(expected), "{hooked}");
        // The hosting gql:filter stands after the element's as().
        let plan = steps(&profile(&graph, hooked));
        let host = plan
            .iter()
            .position(|s| s.starts_with("gql:filter("))
            .unwrap_or_else(|| panic!("{hooked}: no gql:filter in {plan:?}"));
        assert!(
            plan[..host].iter().any(|s| s.starts_with("as(")),
            "{hooked}: {plan:?}"
        );
    }
}

// ----- stage 2d.T5c: COUNT { } of MATCH clauses, the correlated lookup in a body -----

/// `COUNT { }` over `MATCH` clauses (user decision D23) is a counted run on the 2b hook: the
/// body closed by `count()` (bulk counted), `0` when nothing matches or an import is `NULL`;
/// twins against Gremlin `local(__..count())` and the `OPTIONAL MATCH .. count()` form.
#[cfg(feature = "script")]
mod count_twins {
    use super::{modern, norm, rows};

    fn values(script: &str) -> Vec<String> {
        let graph = modern();
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, script)
            .unwrap_or_else(|e| panic!("`{script}` failed to evaluate: {e}"));
        let lock = graph.read();
        let mut out: Vec<String> = gts
            .ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{script}: {e}"))
            .iter()
            .map(norm)
            .collect();
        out.sort();
        out
    }

    fn twin(gql: &str, gremlin: &str) {
        let left = values(&format!("g.gql({gql:?}).select(\"x\")"));
        assert!(!left.is_empty(), "{gql}: no rows to compare");
        assert_eq!(left, values(gremlin), "\nGQL:  {gql}\ntwin: {gremlin}");
    }

    #[test]
    fn count_is_local_count() {
        for (gql, gremlin) in [
            (
                "MATCH (p:person) RETURN COUNT { (p)-->() } AS x",
                r#"g.V().hasLabel("person").local(__.out().count())"#,
            ),
            (
                "MATCH (p:person) RETURN COUNT { MATCH (p)-[:knows]->(f)-[:created]->(s) } AS x",
                r#"g.V().hasLabel("person").local(__.out("knows").out("created").count())"#,
            ),
            // Either direction: a self-loop would count once; trail within the clause.
            (
                "MATCH (p:person) RETURN COUNT { (p)-[]-() } AS x",
                r#"g.V().hasLabel("person").local(__.both().count())"#,
            ),
            // A condition in the body; an inline property.
            (
                "MATCH (p:person) RETURN COUNT { MATCH (p)-->(f) WHERE f.age > 26 } AS x",
                r#"g.V().hasLabel("person").local(__.out().has("age", P.gt(26)).count())"#,
            ),
            (
                "MATCH (s:software) RETURN COUNT { (s)<-[:created]-(:person {name: 'josh'}) } AS x",
                r#"g.V().hasLabel("software").local(__.in("created").has("name", "josh").count())"#,
            ),
            // Uncorrelated: the whole graph per row.
            (
                "MATCH (s:software) RETURN COUNT { MATCH (a)-[:knows]->(b) } AS x",
                r#"g.V().hasLabel("software").local(__.V().out("knows").count())"#,
            ),
            // In a condition (the hook in `gql:filter`).
            (
                "MATCH (p:person) WHERE COUNT { (p)-->() } >= 2 RETURN p.name AS x",
                r#"g.V().hasLabel("person").where(__.out().count().is(P.gte(2))).values("name")"#,
            ),
        ] {
            twin(gql, gremlin);
        }
    }

    /// The `OPTIONAL MATCH .. count()` form (the advice before 2d.T5c) gives the same rows.
    #[test]
    fn count_is_the_optional_match_count_form() {
        for (subquery, form) in [
            (
                "MATCH (p:person) RETURN p.name AS n, COUNT { (p)-[:created]->() } AS c",
                "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name AS n, count(s) AS c",
            ),
            (
                "MATCH (p:person) RETURN p.name AS n, COUNT { MATCH (p)-[:knows]->(f) WHERE f.age > 30 } AS c",
                "MATCH (p:person) OPTIONAL MATCH (p)-[:knows]->(f) WHERE f.age > 30 RETURN p.name AS n, count(f) AS c",
            ),
        ] {
            let mut left: Vec<String> = rows(&modern(), subquery).iter().map(norm).collect();
            let mut right: Vec<String> = rows(&modern(), form).iter().map(norm).collect();
            left.sort();
            right.sort();
            assert_eq!(left, right, "{subquery}");
        }
    }
}

/// `COUNT { }` anywhere an expression stands: a `WHERE` condition, `ORDER BY`, an aggregate's
/// argument, a `FOR` list, a `LET`, nested in another body; a `NULL` import counts `0`.
#[test]
fn count_runs_in_every_host_and_a_null_import_counts_zero() {
    let graph = modern();
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, COUNT { (p)-[:created]->() } AS c",
            false
        ),
        strings(&[
            "n='marko' c=i:1",
            "n='vadas' c=i:0",
            "n='josh' c=i:2",
            "n='peter' c=i:1"
        ])
    );
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE COUNT { MATCH (p)-[:knows]->(f) } > 1 RETURN p.name AS n"
        ),
        sorted(&["marko"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n ORDER BY COUNT { (p)--() } DESC, n",
            true
        ),
        ["n='josh'", "n='marko'", "n='peter'", "n='vadas'"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN sum(COUNT { (p)-->() }) AS t",
            false
        ),
        ["t=i:6"]
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person {name: 'marko'}) FOR x IN [COUNT { (p)-->() }, 0] RETURN x",
            false
        ),
        strings(&["x=i:0", "x=i:3"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) LET c = COUNT { (p)-->() } FILTER c > 0 RETURN p.name AS n, c",
            false
        ),
        strings(&["n='marko' c=i:3", "n='josh' c=i:2", "n='peter' c=i:1"])
    );
    // Nested: a COUNT inside an EXISTS body, an EXISTS inside a COUNT body.
    assert_eq!(
        names(
            &graph,
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(f) WHERE COUNT { (f)-->() } = 2 } \
             RETURN p.name AS n"
        ),
        sorted(&["marko"])
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN p.name AS n, COUNT { MATCH (p)-->(f) WHERE EXISTS { \
             (f)-[:created]->() } } AS c",
            false
        ),
        strings(&[
            "n='marko' c=i:1",
            "n='vadas' c=i:0",
            "n='josh' c=i:0",
            "n='peter' c=i:0"
        ])
    );
    // A NULL import (an OPTIONAL MATCH that did not match) counts 0, never NULL.
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) WHERE s.lang = 'none' \
             RETURN p.name AS n, COUNT { (s)<-[:created]-() } AS c",
            false
        ),
        strings(&[
            "n='marko' c=i:0",
            "n='vadas' c=i:0",
            "n='josh' c=i:0",
            "n='peter' c=i:0"
        ])
    );
}

/// Bulk is counted: merged rows count like unmerged ones, and a body run on a merged row
/// counts the row's own matches once (not multiplied by the row's bulk).
#[test]
fn count_counts_bulk_and_merged_rows_like_unmerged_ones() {
    let graph = modern();
    let graph = graph.read();
    for query in [
        "MATCH ()<-[:knows]-()-[:created]->(s) RETURN s.name AS n, COUNT { (s)<-[:created]-() } AS c",
        "MATCH (a)-->(b) RETURN COUNT { (b)<-[]-() } AS c, count(*) AS k",
        "MATCH (p:person) RETURN COUNT { MATCH (p)-->(a)-->(b) } AS c",
        "MATCH ()-->(b) RETURN sum(COUNT { MATCH (b)<--()-->() }) AS c",
    ] {
        let run = |merge: bool| {
            let mut out: Vec<String> = graph
                .traversal()
                .with("bulk.merge", merge)
                .gql(query)
                .to_list()
                .unwrap_or_else(|e| panic!("{query}: {e}"))
                .iter()
                .map(norm)
                .collect();
            out.sort();
            out
        };
        assert_eq!(run(true), run(false), "{query}");
    }
    // lop is reached twice through marko's two `knows` rows (merged into one traverser of
    // bulk 2): each row counts lop's three creators.
    let rows = graph
        .traversal()
        .with("bulk.merge", true)
        .gql("MATCH ()<-[:knows]-()-[:created]->(s) RETURN s.name AS n, COUNT { (s)<-[:created]-() } AS c")
        .to_list()
        .unwrap();
    let mut rows: Vec<String> = rows.iter().map(norm).collect();
    rows.sort();
    assert_eq!(rows, ["{c: i:3, n: 'lop'}", "{c: i:3, n: 'lop'}"]);
}

/// The plan: the body's child closes with `count()` under the hosting step; the optimizer may
/// fuse it (`count_pushdown`: an uncorrelated labelled scan counts without walking).
#[test]
fn count_is_a_child_closed_by_count() {
    let graph = modern();
    let text = profile(
        &graph,
        "MATCH (p:person) RETURN p.name AS n, COUNT { (p)-[:created]->() } AS c",
    );
    assert_eq!(
        steps(&text),
        [
            r#"v(labels: ["person"])"#,
            r#"gql:project("n", "c")"#,
            r#"\> out("created")"#,
            r#"\> count()"#,
        ],
        "{text}"
    );
    let text = profile(
        &graph,
        "MATCH (p:person) RETURN COUNT { MATCH (s:software) } AS c",
    );
    assert!(
        steps(&text).contains(&r#"\> v(labels: ["software"]).count()"#.to_string()),
        "{text}"
    );
    assert_eq!(
        table(
            &graph,
            "MATCH (p:person) RETURN COUNT { MATCH (s:software) } AS c",
            false
        ),
        ["c=i:2"; 4]
    );
}

/// Stage 2d.T5c (P0 "After T3"): the first component of a body of `MATCH` clauses (`EXISTS`,
/// a pattern predicate, a pattern comprehension, `COUNT { }`) starts with the correlated
/// `gql:lookup` when the id or a property index serves an equality reading an outer binding,
/// as in a `CALL` body; without an index it stays the child-leading scan. Same rows either way.
#[test]
fn a_match_body_uses_the_correlated_lookup() {
    use graphersal::catalog::PropertyIndexDefinition;
    use graphersal::storage::ElementKind;

    let mut indexed = TraversalGraph::tinkerpop_modern();
    indexed
        .traversal_mut()
        .create_index(
            "person_age",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
        )
        .unwrap();
    let indexed = Arc::new(Graph::new(indexed));
    let plain = modern();
    for (query, expected) in [
        (
            "MATCH (p:person) WHERE EXISTS { MATCH (q:person) WHERE q.age = p.age + 3 } \
             RETURN p.name AS n",
            &["n='marko'", "n='josh'"][..],
        ),
        (
            "MATCH (p:person) WHERE p.age > 100 OR EXISTS { MATCH (q:person) WHERE q.age = \
             p.age + 3 } RETURN p.name AS n",
            &["n='marko'", "n='josh'"][..],
        ),
        (
            "MATCH (p:person) RETURN p.name AS n, [(q:person)-->() WHERE q.age = p.age + 3 | \
             q.name] AS l",
            &[
                "n='marko' l=[\"josh\", \"josh\"]",
                "n='vadas' l=[]",
                "n='josh' l=[\"peter\"]",
                "n='peter' l=[]",
            ][..],
        ),
        (
            "MATCH (p:person) RETURN p.name AS n, COUNT { MATCH (q:person) WHERE q.age = p.age \
             + 3 } AS c",
            &[
                "n='marko' c=i:1",
                "n='vadas' c=i:0",
                "n='josh' c=i:1",
                "n='peter' c=i:0",
            ][..],
        ),
    ] {
        let lookup = profile(&indexed, query);
        assert!(lookup.contains("gql:lookup("), "{query}: {lookup}");
        assert!(
            lookup.contains("[index: vertex person.age]"),
            "{query}: {lookup}"
        );
        let scan = profile(&plain, query);
        assert!(!scan.contains("gql:lookup("), "{query}: {scan}");
        assert!(scan.contains(r#"v(labels: ["person"])"#), "{query}: {scan}");
        assert_eq!(table(&indexed, query, false), strings(expected), "{query}");
        assert_eq!(table(&plain, query, false), strings(expected), "{query}");
    }
    // A constant equality keeps the child-leading indexed scan (the lookup serves only an
    // equality reading the row).
    let query = "MATCH (s:software) WHERE EXISTS { MATCH (q:person) WHERE q.age = 32 } \
                 RETURN s.name AS n";
    let text = profile(&indexed, query);
    assert!(!text.contains("gql:lookup("), "{text}");
    assert!(text.contains("[index:"), "{text}");
    // An equality on the element id is served by the id.
    let query = "MATCH (p:person)-[:knows]->(f) WITH p, element_id(f) AS i RETURN p.name AS n, \
                 COUNT { MATCH (q) WHERE element_id(q) = i } AS c";
    let text = profile(&plain, query);
    assert!(text.contains("gql:lookup("), "{text}");
    assert_eq!(
        table(&plain, query, false),
        strings(&["n='marko' c=i:1", "n='marko' c=i:1"])
    );
}
