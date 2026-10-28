//! GQL stage 1b quantified edges (feature `gql`): `{m,n}`, `{m,}`, `{,n}`, `{n}`, `*`, `+` and
//! Cypher's `[*]`, `[*n]`, `[*m..n]`, lowered to `repeat()` with an internal `gql:trail` check
//! (plan mechanism 5). Differential tests pair queries on the (acyclic) modern graph with a
//! hand-written `repeat()..emit(loops)..times()` twin; on a cyclic fixture (a cycle, parallel
//! edges, a self-loop) the rows are compared with every trail a brute-force walk enumerates.
//! Reaching `repeat.max_loops` or `traversal.max_traversers` is an error, never a cut result
//! (design D14), and the plan records labels only, never a full path.
#![cfg(all(feature = "gql", feature = "script"))]

use std::sync::Arc;

use graphersal::prelude::*;

fn rows(graph: &Arc<Graph>, query: &str) -> Vec<ElementProperty> {
    graph
        .read()
        .traversal()
        .gql(query)
        .to_list()
        .unwrap_or_else(|e| panic!("{query}: {e}"))
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

fn column<'r>(row: &'r ElementProperty, key: &str) -> &'r ElementProperty {
    match row {
        ElementProperty::Object(map) => map
            .get(key)
            .unwrap_or_else(|| panic!("no column {key} in {row:?}")),
        other => panic!("not a row map: {other:?}"),
    }
}

/// A column as text: a string, a number, `null`, or a list of edge names `[e1 e2]`.
fn text(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => s.to_string(),
        ElementProperty::Number(n) => format!("{}", n.as_i64().unwrap_or_default()),
        ElementProperty::Array(items) => {
            let names: Vec<String> = items.iter().map(text).collect();
            format!("[{}]", names.join(" "))
        }
        ElementProperty::Edge(edge) => match edge.properties().get("name") {
            Some(ElementProperty::String(name)) => name.to_string(),
            _ => format!("e{}", edge.id()),
        },
        other => format!("{other:?}"),
    }
}

/// The rows as `a|b|..` texts of `keys`, sorted (a multiset, design D18).
fn table(rows: &[ElementProperty], keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|row| {
            keys.iter()
                .map(|k| text(column(row, k)))
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect();
    out.sort();
    out
}

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

// ----- the cyclic trail fixture and its brute-force trails -----

/// The edges of the trail fixture: (name, from, to, label). A `knows` cycle x -> y -> z -> x,
/// a parallel edge x -> y, a self-loop on z and an edge into a sink w.
const EDGES: &[(&str, &str, &str, &str)] = &[
    ("e1", "x", "y", "knows"),
    ("e2", "x", "y", "knows"),
    ("e3", "y", "z", "knows"),
    ("e4", "z", "x", "knows"),
    ("e5", "z", "z", "likes"),
    ("e6", "y", "w", "likes"),
];
const VERTICES: &[&str] = &["x", "y", "z", "w"];

fn cycle() -> Arc<Graph> {
    let graph = Arc::new(Graph::new(TraversalGraph::new()));
    let mut script = String::new();
    for v in VERTICES {
        script.push_str(&format!(
            r#"g.addV("node").property("name", "{v}").iterate();"#
        ));
    }
    for (name, from, to, label) in EDGES {
        script.push_str(&format!(
            r#"g.V().has("name", "{from}").as("f").V().has("name", "{to}").addE("{label}").from("f").property("name", "{name}").iterate();"#
        ));
    }
    let _ = crate::script_host::eval_value(graph.clone(), &script).unwrap();
    graph
}

#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Out,
    In,
    Any,
}

/// Every trail (no edge twice) from every vertex with `min..=max` edges (`max` `None`:
/// unbounded) over the edges whose label is in `labels` (empty: every edge), walked in `dir`;
/// `banned` edges are used already. Rows: (start, end, edges in walk order).
fn trails(
    dir: Dir,
    labels: &[&str],
    min: usize,
    max: Option<usize>,
    starts: &[&str],
    banned: &[&str],
) -> Vec<(String, String, Vec<String>)> {
    fn walk(
        at: &str,
        start: &str,
        used: &mut Vec<String>,
        args: (Dir, &[&str], usize, Option<usize>),
        out: &mut Vec<(String, String, Vec<String>)>,
        banned: &[&str],
    ) {
        let (dir, labels, min, max) = args;
        let depth = used.len() - banned.len();
        if depth >= min {
            out.push((
                start.to_string(),
                at.to_string(),
                used[banned.len()..].to_vec(),
            ));
        }
        if max.is_some_and(|m| depth >= m) {
            return;
        }
        for (name, from, to, label) in EDGES {
            if used.iter().any(|u| u == name) || !(labels.is_empty() || labels.contains(label)) {
                continue;
            }
            // Either direction binds a self-loop once (the outgoing branch).
            let next = match dir {
                Dir::Out => (*from == at).then_some(*to),
                Dir::In => (*to == at).then_some(*from),
                Dir::Any if *from == at => Some(*to),
                Dir::Any => (*to == at).then_some(*from),
            };
            if let Some(next) = next {
                used.push(name.to_string());
                walk(next, start, used, args, out, banned);
                used.pop();
            }
        }
    }
    let mut out = Vec::new();
    for start in starts {
        let mut used: Vec<String> = banned.iter().map(|b| b.to_string()).collect();
        walk(
            start,
            start,
            &mut used,
            (dir, labels, min, max),
            &mut out,
            banned,
        );
    }
    out
}

fn trail_table(trails: &[(String, String, Vec<String>)]) -> Vec<String> {
    let mut out: Vec<String> = trails
        .iter()
        .map(|(a, b, es)| format!("{a}|{b}|[{}]", es.join(" ")))
        .collect();
    out.sort();
    out
}

/// A query and the trails it returns: direction, edge labels, bounds.
type Case = (
    &'static str,
    Dir,
    &'static [&'static str],
    usize,
    Option<usize>,
);

#[test]
fn every_trail_of_a_cyclic_graph() {
    let graph = cycle();
    let cases: &[Case] = &[
        // GQL `*` is `{0,}`: every trail, the zero-length ones included.
        (
            "MATCH (a)-[e]->*(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            0,
            None,
        ),
        (
            "MATCH (a)-[e]->+(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            1,
            None,
        ),
        (
            "MATCH (a)-[e]->{1,3}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            1,
            Some(3),
        ),
        (
            "MATCH (a)-[e]->{2,}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            2,
            None,
        ),
        (
            "MATCH (a)-[e]->{,2}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            0,
            Some(2),
        ),
        (
            "MATCH (a)-[e]->{3}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            3,
            Some(3),
        ),
        (
            "MATCH (a)-[e:knows]->{2,3}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &["knows"],
            2,
            Some(3),
        ),
        (
            "MATCH (a)<-[e]-{1,}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::In,
            &[],
            1,
            None,
        ),
        (
            "MATCH (a)<-[e:knows|likes]-{2,4}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::In,
            &["knows", "likes"],
            2,
            Some(4),
        ),
        (
            "MATCH (a)-[e]-{1,2}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Any,
            &[],
            1,
            Some(2),
        ),
        (
            "MATCH (a)-[e]-*(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Any,
            &[],
            0,
            None,
        ),
        // Cypher: `[*]` is `{1,}`, `[*2]` is `{2}`, `[*..2]` is `{1,2}`, `[*2..]` is `{2,}`.
        (
            "MATCH (a)-[e*]->(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            1,
            None,
        ),
        (
            "MATCH (a)-[e*2]->(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            2,
            Some(2),
        ),
        (
            "MATCH (a)-[e*..2]->(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            1,
            Some(2),
        ),
        (
            "MATCH (a)-[e:knows*2..]->(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &["knows"],
            2,
            None,
        ),
        // An upper bound beyond the loop counter is no bound.
        (
            "MATCH (a)-[e]->{1,99999999999}(b) RETURN a.name AS a, b.name AS b, e",
            Dir::Out,
            &[],
            1,
            None,
        ),
    ];
    for (query, dir, labels, min, max) in cases {
        let expected = trail_table(&trails(*dir, labels, *min, *max, VERTICES, &[]));
        assert!(!expected.is_empty(), "{query}");
        assert_eq!(
            table(&rows(&graph, query), &["a", "b", "e"]),
            expected,
            "{query}"
        );
    }
    // The whole cycle, through either parallel edge, as one trail of three edges.
    let cycles = table(
        &rows(
            &graph,
            "MATCH (a {name: 'x'})-[e:knows]->{1,}(a) RETURN a.name AS a, a.name AS b, e",
        ),
        &["a", "b", "e"],
    );
    assert_eq!(cycles, ["x|x|[e1 e3 e4]", "x|x|[e2 e3 e4]"]);
}

#[test]
fn joins_after_the_loop_and_trail_with_the_other_edges_of_the_clause() {
    let graph = cycle();
    // A closing join: every trail that returns to its start.
    let expected: Vec<String> = trail_table(
        &trails(Dir::Out, &[], 1, None, VERTICES, &[])
            .into_iter()
            .filter(|(a, b, _)| a == b)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a)-[e]->+(a) RETURN a.name AS a, a.name AS b, e"
            ),
            &["a", "b", "e"]
        ),
        expected
    );
    // A single edge before the group: the group never reuses it, in either walk order.
    let mut expected = Vec::new();
    for (f, from, to, _) in EDGES {
        for (_, c, es) in trails(Dir::Out, &[], 1, Some(2), &[to], &[f]) {
            expected.push(format!("{from}|{f}|[{}]|{c}", es.join(" ")));
        }
    }
    expected.sort();
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a)-[f]->(b)-[e]->{1,2}(c) RETURN a.name AS a, f, e, c.name AS c"
            ),
            &["a", "f", "e", "c"]
        ),
        expected
    );
    // A single edge after the group (walked after it: plain edges rank first, but only the
    // group has a bound end here): the single edge checks every iteration with gql:trail.
    let mut expected = Vec::new();
    for (a, b, es) in trails(Dir::Out, &[], 1, Some(2), &["x"], &[]) {
        for (f, from, to, _) in EDGES {
            if *from == b && !es.iter().any(|e| e == f) {
                expected.push(format!("{a}|[{}]|{f}|{to}", es.join(" ")));
            }
        }
    }
    expected.sort();
    let query =
        "MATCH (a {name: 'x'})-[e]->{1,2}(b)-[f]->(c) RETURN a.name AS a, e, f, c.name AS c";
    assert_eq!(table(&rows(&graph, query), &["a", "e", "f", "c"]), expected);
    let plan = profile(&graph, query);
    assert!(plan.contains("gql:trail(\"e\")"), "{plan}");
    // Two groups in one clause: no edge in both.
    let both = rows(
        &graph,
        "MATCH (a {name: 'x'})-[e:knows]->{1,2}(b)-[g:knows]->{1,2}(c) RETURN e, g",
    );
    assert!(!both.is_empty());
    for row in &both {
        let (e, g) = (text(column(row, "e")), text(column(row, "g")));
        let e: Vec<&str> = e.trim_matches(['[', ']']).split(' ').collect();
        assert!(
            g.trim_matches(['[', ']'])
                .split(' ')
                .all(|x| !e.contains(&x)),
            "{e:?} {g}"
        );
    }
}

#[test]
fn zero_length_and_huge_bounds() {
    let graph = cycle();
    // `{0}` and Cypher `*0`: only the zero-length match, the far node is the near one.
    for query in [
        "MATCH (a)-[e]->{0}(b) RETURN a.name AS a, b.name AS b, e",
        "MATCH (a)-[e]->{0,0}(b) RETURN a.name AS a, b.name AS b, e",
        "MATCH (a)-[e*0]->(b) RETURN a.name AS a, b.name AS b, e",
    ] {
        assert_eq!(
            table(&rows(&graph, query), &["a", "b", "e"]),
            ["w|w|[]", "x|x|[]", "y|y|[]", "z|z|[]"],
            "{query}"
        );
        let plan = profile(&graph, query);
        assert!(!plan.contains("repeat("), "{plan}");
    }
    // A label filter on the far node of `{0}` runs on the near one.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a)-[:knows]->{0}(b {name: 'z'}) RETURN a.name AS a"
            ),
            &["a"]
        ),
        ["z"]
    );
    // A lower bound no trail reaches: no rows, no error.
    assert!(rows(&graph, "MATCH (a)-[e]->{99999999999,}(b) RETURN b").is_empty());
    assert!(rows(&graph, "MATCH (a)-[e]->{7,99999999999}(b) RETURN b").is_empty());
}

#[test]
fn group_variables_read_as_lists_in_text_order() {
    let graph = modern();
    // Walked from the written target (the software is more selective): still text order.
    let query = "MATCH (a)-[e]->{1,2}(b:software {name: 'ripple'}) RETURN a.name AS a, e";
    assert_eq!(
        table(&rows(&graph, query), &["a", "e"]),
        ["josh|[e4]", "marko|[e2 e4]"]
    );
    assert!(profile(&graph, query).contains("in_e()"));
    // size(), a WHERE on the group, an inline condition per edge.
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a {name: 'marko'})-[e]->{0,2}(b) WHERE size(e) = 2 RETURN b.name AS b"
            ),
            &["b"]
        ),
        ["lop", "ripple"]
    );
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a {name: 'marko'})-[e WHERE e.weight >= 1.0]->{1,2}(b) RETURN b.name AS b, size(e) AS n"
            ),
            &["b", "n"]
        ),
        ["josh|1", "ripple|2"]
    );
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a {name: 'marko'})-[e {weight: 1.0}]->{1,2}(b) RETURN b.name AS b"
            ),
            &["b"]
        ),
        ["josh", "ripple"]
    );
}

#[test]
fn quantified_edges_inside_optional_match() {
    let graph = cycle();
    // A part with a new node: a row it does not match reads NULL, a zero-length match [].
    let query =
        "MATCH (a) OPTIONAL MATCH (a)-[e:likes]->{1,2}(b) RETURN a.name AS a, b.name AS b, e";
    assert_eq!(
        table(&rows(&graph, query), &["a", "b", "e"]),
        ["w|null|null", "x|null|null", "y|w|[e6]", "z|z|[e5]"]
    );
    let query =
        "MATCH (a) OPTIONAL MATCH (a)-[e:likes]->{0,1}(b) RETURN a.name AS a, b.name AS b, e";
    assert_eq!(
        table(&rows(&graph, query), &["a", "b", "e"]),
        [
            "w|w|[]", "x|x|[]", "y|w|[e6]", "y|y|[]", "z|z|[]", "z|z|[e5]"
        ]
    );
    // A part that binds no new node (T5): its part label tells "not matched" (NULL) from a
    // zero-length match ([]).
    let query = "MATCH (a) OPTIONAL MATCH (a)-[e:knows]->{1,3}(a) RETURN a.name AS a, e";
    assert_eq!(
        table(&rows(&graph, query), &["a", "e"]),
        [
            "w|null",
            "x|[e1 e3 e4]",
            "x|[e2 e3 e4]",
            "y|[e3 e4 e1]",
            "y|[e3 e4 e2]",
            "z|[e4 e1 e3]",
            "z|[e4 e2 e3]",
        ]
    );
    let query = "MATCH (a) OPTIONAL MATCH (a)-[e:knows]->{0,2}(a) RETURN a.name AS a, e";
    assert_eq!(
        table(&rows(&graph, query), &["a", "e"]),
        ["w|[]", "x|[]", "y|[]", "z|[]"]
    );
    let query = "MATCH (a) OPTIONAL MATCH (a)-[e:knows]->{1,2}(a) RETURN a.name AS a, e";
    assert_eq!(
        table(&rows(&graph, query), &["a", "e"]),
        ["w|null", "x|null", "y|null", "z|null"]
    );
    let plan = profile(&graph, query);
    assert!(plan.contains("as(\"#o1\")"), "{plan}");
    // Not read after the part: no part label.
    let plan = profile(
        &graph,
        "MATCH (a) OPTIONAL MATCH (a)-[e:knows]->{1,2}(a) RETURN a.name AS a",
    );
    assert!(!plan.contains("#o1"), "{plan}");
    // Read inside the part (its WHERE), and after it.
    let query = "MATCH (a {name: 'x'}) OPTIONAL MATCH (a)-[e:knows]->{1,3}(b) WHERE size(e) = 2 \
                 RETURN b.name AS b, e";
    assert_eq!(
        table(&rows(&graph, query), &["b", "e"]),
        ["z|[e1 e3]", "z|[e2 e3]"]
    );
}

#[test]
fn reaching_a_limit_is_an_error_never_a_cut_result() {
    let graph = cycle();
    let all = trails(Dir::Any, &[], 1, None, VERTICES, &[]).len();
    let query = "MATCH (a)-[e]-{1,}(b) RETURN count(*) AS n";
    let lock = graph.read();
    // The longest trail has 6 edges: a loop limit of 7 suffices, 3 does not.
    let n = lock
        .traversal()
        .with("repeat.max_loops", 7)
        .gql(query)
        .select("n")
        .next()
        .unwrap();
    assert_eq!(n, Some(ElementProperty::from(all as i64)));
    let error = lock
        .traversal()
        .with("repeat.max_loops", 3)
        .gql(query)
        .to_list()
        .expect_err("an unbounded quantifier past repeat.max_loops fails");
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::RepeatLimitExceeded { max_loops: 3, .. }
        ),
        "{error}"
    );
    // Also when the rows are listed (no count), and inside OPTIONAL MATCH.
    for query in [
        "MATCH (a)-[e]->*(b) RETURN a, b, e",
        "MATCH (a) OPTIONAL MATCH (a)-[e]-+(b) RETURN a, e",
    ] {
        let error = lock
            .traversal()
            .with("repeat.max_loops", 2)
            .gql(query)
            .to_list()
            .expect_err(query);
        assert!(
            matches!(
                error.root_cause(),
                TraverserError::RepeatLimitExceeded { .. }
            ),
            "{query}: {error}"
        );
    }
    // A bounded quantifier whose trails outgrow traversal.max_traversers.
    let error = lock
        .traversal()
        .with("traversal.max_traversers", 10)
        .gql("MATCH (a)-[e]-{1,4}(b) RETURN a, b, e")
        .to_list()
        .expect_err("too many trails");
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::ResourceLimitExceeded { limit, .. } if limit == "traversal.max_traversers"
        ),
        "{error}"
    );
}

#[test]
fn the_plan_records_labels_only_never_a_full_path() {
    let graph = cycle();
    for query in [
        "MATCH (a)-[e]->{1,3}(b) RETURN b",
        "MATCH (a)-[e]->{1,3}(b) RETURN e",
        "MATCH (a)-[e]-*(b) RETURN a, e",
        "MATCH (a)<-[:knows]-{2,}(a) RETURN a",
        "MATCH (a)-[f]->(b)-[e]->{1,2}(c) RETURN f, e",
        "MATCH (a) OPTIONAL MATCH (a)-[e]->{0,2}(a) RETURN a, e",
        "MATCH (a) OPTIONAL MATCH (a)-[e]->{1,2}(b) RETURN b, e",
        "MATCH (a)-[e*1..2]->(b) WITH b, count(*) AS n MATCH (b)-[g]->+(c) RETURN n, g",
    ] {
        let plan = profile(&graph, query);
        assert!(plan.contains("repeat("), "{query}\n{plan}");
        assert!(plan.contains("gql:trail("), "{query}\n{plan}");
        assert!(plan.contains("[path: labels("), "{query}\n{plan}");
        assert!(!plan.contains("full"), "{query}\n{plan}");
    }
    // The forms of the bounds.
    let forms = [
        ("{1,3}", "times(3).emit()"),
        ("{2,3}", "times(3).emit(__.l"),
        ("{2}", "times(2)"),
        ("{2,3}", "is(P.gte(2))"),
        ("{0,2}", "emit().repeat("),
        ("*", "emit().repeat("),
        ("+", ".emit()"),
    ];
    for (quantifier, form) in forms {
        let plan = profile(&graph, &format!("MATCH (a)-[e]->{quantifier}(b) RETURN b"));
        assert!(plan.contains(form), "{quantifier}: {plan}");
    }
}

mod differential {
    use std::sync::Arc;

    use graphersal::prelude::*;

    /// Runs the GQL query and its Gremlin twin on `graph`; compares the rows as multisets.
    fn same(graph: &Arc<Graph>, query: &str, twin: &str) {
        let lock = graph.read();
        let left = lock
            .traversal()
            .gql(query)
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        drop(lock);
        let engine = crate::script_host::engine();
        let mut scope = crate::script_host::graph_scope(graph.clone());
        let gts = engine
            .eval_with_scope::<crate::script_host::ScriptGraphTraversalSource>(&mut scope, twin)
            .unwrap_or_else(|e| panic!("`{twin}` failed to evaluate: {e}"));
        let lock = graph.read();
        let right = gts
            .ast_convert_to_gts(&*lock)
            .to_list()
            .unwrap_or_else(|e| panic!("{twin}: {e}"));
        let text = |rows: &[ElementProperty]| -> Vec<String> {
            let mut out: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
            out.sort();
            out
        };
        assert_eq!(text(&left), text(&right), "\nGQL:  {query}\ntwin: {twin}");
        assert!(!left.is_empty(), "{query}: no rows to compare");
    }

    /// On the modern graph every directed walk is a trail (it has no directed cycle), so
    /// `repeat()` without a trail check is the twin of a directed quantified edge.
    #[test]
    fn directed_quantifiers_match_their_repeat_twins() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let marko = r#"g.V().has("name", "marko")"#;
        let cases: Vec<(&str, String)> = vec![
            (
                "MATCH (a {name: 'marko'})-[:knows|created]->{1,2}(b) RETURN b.name AS n",
                format!(
                    r#"{marko}.repeat(__.out("knows", "created")).emit(__.loops().is(P.gte(1))).times(2).project("n").by("name")"#
                ),
            ),
            (
                "MATCH (a {name: 'marko'})-->{0,2}(b) RETURN b.name AS n",
                format!(r#"{marko}.emit().repeat(__.out()).times(2).project("n").by("name")"#),
            ),
            (
                "MATCH (a {name: 'marko'})-->{2}(b) RETURN b.name AS n",
                format!(r#"{marko}.repeat(__.out()).times(2).project("n").by("name")"#),
            ),
            (
                "MATCH (a {name: 'marko'})-->{2,3}(b) RETURN b.name AS n",
                format!(
                    r#"{marko}.repeat(__.out()).emit(__.loops().is(P.gte(2))).times(3).project("n").by("name")"#
                ),
            ),
            (
                "MATCH (a:person)-[:knows]->*(b) RETURN a.name AS a, b.name AS b",
                r#"g.V().hasLabel("person").as("a").emit().repeat(__.out("knows")).project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
            (
                "MATCH (a)-[*]->(b) RETURN a.name AS a, b.name AS b",
                r#"g.V().as("a").repeat(__.out()).emit().project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
            (
                "MATCH (s:software)<-[:created*1..2]-(p) RETURN s.name AS a, p.name AS b",
                r#"g.V().hasLabel("software").as("a").repeat(__.in("created")).emit().times(2).project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
            (
                "MATCH (a {name: 'marko'})-[e WHERE e.weight > 0.4]->{1,2}(b) RETURN b.name AS n",
                format!(
                    r#"{marko}.repeat(__.outE().has("weight", P.gt(0.4)).inV()).emit().times(2).project("n").by("name")"#
                ),
            ),
            (
                "MATCH (a {name: 'marko'})-[e]->{1,2}(b) RETURN b.name AS n, e",
                format!(
                    r#"{marko}.repeat(__.outE().as("q").inV()).emit().times(2).project("n", "e").by("name").by(__.select(Pop.all, "q"))"#
                ),
            ),
            (
                "MATCH (a)-[:created]->{1,2}(b) WHERE a.age > 30 RETURN a.name AS a, b.name AS b",
                r#"g.V().has("age", P.gt(30)).as("a").repeat(__.out("created")).emit().times(2).project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
        ];
        for (query, twin) in &cases {
            same(&graph, query, twin);
        }
    }

    /// The `knows` edges of the modern graph form a tree, so a trail in either direction is a
    /// simple path there: `simplePath()` is the twin of the trail check.
    #[test]
    fn either_direction_matches_a_simple_path_twin_on_a_tree() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        same(
            &graph,
            "MATCH (a:person)-[:knows]-{1,2}(b) RETURN a.name AS a, b.name AS b",
            r#"g.V().hasLabel("person").as("a").repeat(__.both("knows").simplePath()).emit().times(2).project("a", "b").by(__.select("a").values("name")).by("name")"#,
        );
        same(
            &graph,
            "MATCH (a {name: 'vadas'})-[:knows]-*(b) RETURN b.name AS b",
            r#"g.V().has("name", "vadas").emit().repeat(__.both("knows").simplePath()).project("b").by("name")"#,
        );
    }
}
