//! GQL stage 2b `EXISTS` and pattern predicates, the fast path (feature `gql`): a top-level
//! `[NOT] EXISTS` conjunct of `WHERE` / `FILTER` / an inline `WHERE` (a Cypher pattern
//! predicate too) lowers to `filter(__.<body>)` / `not(__.<body>)`, the body planned with its
//! imports as anchors (design M2, E7, E11). Differential twins against Gremlin `where(__..)` /
//! `not(__..)` (Rhai) compare the rows as multisets (design D18); `NULL` imports (E9) and trail
//! inside a body (E8) are hand-computed rows. A subquery anywhere else in an expression stays
//! refused "not yet" (stage 2b.T5).
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
            "filter(__.out(\"knows\").lazy_barrier().out(\"created\").has(\"lang\", P....",
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
            "MATCH (p:person) LET x = (p)-[:created]->() FILTER x RETURN p.name AS n"
        ),
        sorted(&["marko", "josh", "peter"])
    );
    assert!(
        rows(
            &graph,
            "MATCH (p:person) LET x = (p)-[:created]->() FILTER x AND NOT x RETURN p.name AS n"
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

// ----- limits and refusals -----

#[test]
fn exists_elsewhere_in_an_expression_is_not_yet_supported() {
    let graph = modern();
    for (query, text) in [
        (
            "MATCH (p:person) WHERE p.age > 30 OR EXISTS { (p)-->() } RETURN p",
            "EXISTS is not yet supported here",
        ),
        (
            "MATCH (p:person) RETURN EXISTS { (p)-->() } AS x",
            "EXISTS is not yet supported here",
        ),
        (
            "MATCH (p:person) RETURN CASE WHEN (p)-->() THEN 1 END AS x",
            "a pattern predicate (a pattern where an expression stands) is not yet supported here",
        ),
        (
            "MATCH (p:person) RETURN p.name AS n ORDER BY EXISTS { (p)-->() }",
            "EXISTS is not yet supported here",
        ),
        (
            "MATCH (p:person) FOR x IN [EXISTS { (p)-->() }] RETURN x",
            "EXISTS is not yet supported here",
        ),
        (
            "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(q) WHERE q.age > 1 OR (q)-->() } RETURN p",
            "a pattern predicate",
        ),
    ] {
        let err = error(&graph, query);
        assert!(err.contains(text), "{query}: {err}");
        assert!(err.contains("0A000"), "{query}: {err}");
    }
}

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
