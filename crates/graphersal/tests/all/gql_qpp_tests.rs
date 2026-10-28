//! GQL stage 2e parenthesized and quantified path patterns (feature `gql`): `((x)-[e]->(y)
//! WHERE ..){m,n}`, `*`, `+`, `?` (ISO conditional singletons) and no quantifier, lowered to
//! `repeat()` of the body with the modulators of a quantified edge, `gql:trail` per body edge
//! and the body `WHERE` per iteration (design item 9, M5). On a cyclic fixture (a cycle,
//! parallel edges, a self-loop) the rows are compared with every match a brute-force walk
//! enumerates (trail over the whole `MATCH` clause, the merge rule, group lists in text order,
//! path values); on the (acyclic) modern graph queries are paired with hand-written
//! `repeat(__.out()..)` twins. Reaching a limit is an error naming the pattern, never a cut
//! result, and the plan records labels only, never a full path.
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

/// A value as text: a string, a number, `null`, an element by its `name`, a list or a path as
/// `[a b ..]`.
fn text(value: &ElementProperty) -> String {
    match value {
        ElementProperty::Null => "null".to_string(),
        ElementProperty::String(s) => s.to_string(),
        ElementProperty::Boolean(b) => b.to_string(),
        ElementProperty::Number(n) => format!("{}", n.as_i64().unwrap_or_default()),
        ElementProperty::Array(items) => {
            let names: Vec<String> = items.iter().map(text).collect();
            format!("[{}]", names.join(" "))
        }
        ElementProperty::Edge(edge) => match edge.properties().get("name") {
            Some(ElementProperty::String(name)) => name.to_string(),
            _ => format!("e{}", edge.id()),
        },
        ElementProperty::Vertex(vertex) => match vertex.properties().get("name") {
            Some(ElementProperty::String(name)) => name.to_string(),
            _ => format!("v{}", vertex.id()),
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

// ----- the cyclic fixture of `gql_quantifier_tests` and its brute-force matches -----

/// The edges of the fixture: (name, from, to, label). A `knows` cycle x -> y -> z -> x, a
/// parallel edge x -> y, a self-loop on z and an edge into a sink w.
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

#[derive(Clone, Copy, PartialEq, Debug)]
enum Dir {
    Out,
    In,
    Any,
}

/// One hop of a body as written: its direction and its edge labels (empty: any edge).
type Hop = (Dir, &'static [&'static str]);

/// One iteration of a body: its nodes (the hops' count + 1) and its edges, in text order.
type Iteration = (Vec<String>, Vec<String>);

/// A match of `(a) (body){m,n} (b)`: the outer nodes and the iterations in text order.
#[derive(Clone, Debug)]
struct Walk {
    start: String,
    end: String,
    iterations: Vec<Iteration>,
}

impl Walk {
    /// Every edge of the walk.
    fn edges(&self) -> Vec<String> {
        self.iterations
            .iter()
            .flat_map(|(_, e)| e.clone())
            .collect()
    }

    /// The list of body node `node` (index into the body's nodes) over the iterations.
    fn node_list(&self, node: usize) -> String {
        let names: Vec<String> = self
            .iterations
            .iter()
            .map(|(n, _)| n[node].clone())
            .collect();
        format!("[{}]", names.join(" "))
    }

    /// The list of body edge `edge` over the iterations.
    fn edge_list(&self, edge: usize) -> String {
        let names: Vec<String> = self
            .iterations
            .iter()
            .map(|(_, e)| e[edge].clone())
            .collect();
        format!("[{}]", names.join(" "))
    }

    /// The path `[a e1 m f y ..]` in text order (one vertex where two meet).
    fn path(&self) -> String {
        let mut items = vec![self.start.clone()];
        for (nodes, edges) in &self.iterations {
            for (at, edge) in edges.iter().enumerate() {
                items.push(edge.clone());
                items.push(nodes[at + 1].clone());
            }
        }
        format!("[{}]", items.join(" "))
    }
}

/// A body condition over one iteration's nodes and edges.
type Keep<'k> = &'k dyn Fn(&[String], &[String]) -> bool;

/// Every match of a body (`hops` written from its first node) repeated `min..=max` times
/// (`max` `None`: unbounded) from each of `starts`, trail over the walk and `banned` (edges
/// the clause used already), each iteration kept by `keep`. The walk is enumerated in text
/// order; GQL may walk it from either end, the matches are the same.
fn qpp(
    hops: &[Hop],
    (min, max): (usize, Option<usize>),
    starts: &[&str],
    banned: &[&str],
    keep: Keep<'_>,
) -> Vec<Walk> {
    struct Enum<'a> {
        hops: &'a [Hop],
        min: usize,
        max: Option<usize>,
        keep: Keep<'a>,
        out: Vec<Walk>,
    }
    impl Enum<'_> {
        fn iterate(
            &mut self,
            start: &str,
            at: &str,
            used: &mut Vec<String>,
            done: &mut Vec<Iteration>,
        ) {
            if done.len() >= self.min {
                self.out.push(Walk {
                    start: start.to_string(),
                    end: at.to_string(),
                    iterations: done.clone(),
                });
            }
            if self.max.is_some_and(|m| done.len() >= m) {
                return;
            }
            let mut nodes = vec![at.to_string()];
            let mut edges = Vec::new();
            self.hop(0, start, at, used, done, (&mut nodes, &mut edges));
        }

        fn hop(
            &mut self,
            h: usize,
            start: &str,
            at: &str,
            used: &mut Vec<String>,
            done: &mut Vec<Iteration>,
            (nodes, edges): (&mut Vec<String>, &mut Vec<String>),
        ) {
            let Some(&(dir, labels)) = self.hops.get(h) else {
                if (self.keep)(nodes, edges) {
                    done.push((nodes.clone(), edges.clone()));
                    self.iterate(start, at, used, done);
                    done.pop();
                }
                return;
            };
            for (name, from, to, label) in EDGES {
                if used.iter().any(|u| u == name) || !(labels.is_empty() || labels.contains(label))
                {
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
                    nodes.push(next.to_string());
                    edges.push(name.to_string());
                    self.hop(h + 1, start, next, used, done, (nodes, edges));
                    edges.pop();
                    nodes.pop();
                    used.pop();
                }
            }
        }
    }
    let mut walks = Enum {
        hops,
        min,
        max,
        keep,
        out: Vec::new(),
    };
    for start in starts {
        let mut used: Vec<String> = banned.iter().map(|b| b.to_string()).collect();
        walks.iterate(start, start, &mut used, &mut Vec::new());
    }
    walks.out
}

fn sorted(mut rows: Vec<String>) -> Vec<String> {
    rows.sort();
    rows
}

fn any(_: &[String], _: &[String]) -> bool {
    true
}

/// A body of `hops` written as GQL with the variables `x`, `m`, `n`, .. `y` for its nodes and
/// `e`, `f`, `h`, .. for its edges.
fn body_text(hops: &[Hop]) -> String {
    const NODES: [&str; 4] = ["m", "n", "o", "q"];
    const EDGE_NAMES: [&str; 4] = ["e", "f", "h", "k"];
    let mut text = String::from("(x)");
    for (at, (dir, labels)) in hops.iter().enumerate() {
        let labels = if labels.is_empty() {
            String::new()
        } else {
            format!(":{}", labels.join("|"))
        };
        let edge = format!("[{}{labels}]", EDGE_NAMES[at]);
        text.push_str(&match dir {
            Dir::Out => format!("-{edge}->"),
            Dir::In => format!("<-{edge}-"),
            Dir::Any => format!("-{edge}-"),
        });
        if at + 1 == hops.len() {
            text.push_str("(y)");
        } else {
            text.push_str(&format!("({})", NODES[at]));
        }
    }
    text
}

/// The quantifier text and its bounds.
type Bounds = (&'static str, usize, Option<usize>);

const BOUNDS: &[Bounds] = &[
    ("{1,3}", 1, Some(3)),
    ("*", 0, None),
    ("+", 1, None),
    ("{0,2}", 0, Some(2)),
    ("{2,}", 2, None),
    ("{2}", 2, Some(2)),
    ("{,2}", 0, Some(2)),
    ("{0}", 0, Some(0)),
    ("{1,99999999999}", 1, None),
];

const BODIES: &[&[Hop]] = &[
    &[(Dir::Out, &[])],
    &[(Dir::Out, &[]), (Dir::Out, &[])],
    &[(Dir::Out, &["knows"]), (Dir::In, &[])],
    &[(Dir::Any, &[]), (Dir::Out, &["knows"])],
    &[
        (Dir::Out, &[]),
        (Dir::Any, &["knows", "likes"]),
        (Dir::In, &[]),
    ],
];

const EDGE_NAMES: [&str; 3] = ["e", "f", "h"];

#[test]
fn every_match_of_a_quantified_path_pattern_on_a_cyclic_graph() {
    let graph = cycle();
    let mut total = 0;
    for hops in BODIES {
        let body = body_text(hops);
        let edges = &EDGE_NAMES[..hops.len()];
        for &(quantifier, min, max) in BOUNDS {
            let query = format!(
                "MATCH (a) ({body}){quantifier} (b) RETURN a.name AS a, b.name AS b, x, {}",
                edges.join(", ")
            );
            let expected = sorted(
                qpp(hops, (min, max), VERTICES, &[], &any)
                    .iter()
                    .map(|w| {
                        let mut row = vec![w.start.clone(), w.end.clone(), w.node_list(0)];
                        row.extend((0..edges.len()).map(|e| w.edge_list(e)));
                        row.join("|")
                    })
                    .collect(),
            );
            total += expected.len();
            let mut keys = vec!["a", "b", "x"];
            keys.extend(edges);
            assert_eq!(table(&rows(&graph, &query), &keys), expected, "{query}");
        }
    }
    assert!(total > 500, "{total}");
}

#[test]
fn walked_from_the_far_end_the_lists_and_paths_stay_in_text_order() {
    let graph = cycle();
    for hops in BODIES {
        let body = body_text(hops);
        for &(quantifier, min, max) in &BOUNDS[..6] {
            // The equality on `b` makes the planner start there and walk the body backwards.
            let query = format!(
                "MATCH p = (a) ({body}){quantifier} (b:node {{name: 'z'}}) \
                 RETURN a.name AS a, x, y, e, p"
            );
            let expected = sorted(
                qpp(hops, (min, max), VERTICES, &[], &any)
                    .iter()
                    .filter(|w| w.end == "z")
                    .map(|w| {
                        let last = hops.len();
                        format!(
                            "{}|{}|{}|{}|{}",
                            w.start,
                            w.node_list(0),
                            w.node_list(last),
                            w.edge_list(0),
                            w.path()
                        )
                    })
                    .collect(),
            );
            assert_eq!(
                table(&rows(&graph, &query), &["a", "x", "y", "e", "p"]),
                expected,
                "{query}"
            );
            if quantifier != "{0}" {
                let plan = profile(&graph, &query);
                let scan = plan.find("P.eq(\"z\")").unwrap_or(usize::MAX);
                let repeat = plan.find("repeat(").unwrap_or(0);
                assert!(scan < repeat, "walked from b: {query}\n{plan}");
            }
        }
    }
}

#[test]
fn path_variables_assemble_every_iteration_in_text_order() {
    let graph = cycle();
    for hops in BODIES {
        let body = body_text(hops);
        for &(quantifier, min, max) in &BOUNDS[..6] {
            let query =
                format!("MATCH p = (a) ({body}){quantifier} (b) RETURN p, path_length(p) AS n");
            let expected = sorted(
                qpp(hops, (min, max), VERTICES, &[], &any)
                    .iter()
                    .map(|w| format!("{}|{}", w.path(), w.edges().len()))
                    .collect(),
            );
            assert_eq!(
                table(&rows(&graph, &query), &["p", "n"]),
                expected,
                "{query}"
            );
        }
    }
    // A path through two patterns and a plain edge between them.
    let query = "MATCH p = (a {name: 'x'}) ((u)-[e:knows]->(v)){1,2} (c)-[g:likes]->(d) \
                 ((s)-[f]->(t))? (b) RETURN p";
    let mut expected = Vec::new();
    for w in qpp(&[(Dir::Out, &["knows"])], (1, Some(2)), &["x"], &[], &any) {
        for (g, from, to, label) in EDGES {
            if *from != w.end || *label != "likes" {
                continue;
            }
            let mut banned = w.edges();
            banned.push(g.to_string());
            let banned: Vec<&str> = banned.iter().map(String::as_str).collect();
            for tail in qpp(&[(Dir::Out, &[])], (0, Some(1)), &[to], &banned, &any) {
                let head = w.path();
                let rest = tail.path();
                expected.push(format!(
                    "{} {g} {}]",
                    &head[..head.len() - 1],
                    &rest[1..rest.len() - 1]
                ));
            }
        }
    }
    assert!(!expected.is_empty());
    assert_eq!(
        table(&rows(&graph, query), &["p"]),
        sorted(expected),
        "{query}"
    );
}

#[test]
fn trail_covers_the_other_edges_of_the_clause() {
    let graph = cycle();
    let body: &[Hop] = &[(Dir::Out, &[]), (Dir::Out, &[])];
    // An edge walked before the pattern.
    let query = "MATCH (a)-[g]->(c) ((x)-[e]->(m)-[f]->(y)){1,2} (b) \
                 RETURN a.name AS a, g, b.name AS b, e, f";
    let mut expected = Vec::new();
    for (g, from, to, _) in EDGES {
        for w in qpp(body, (1, Some(2)), &[to], &[g], &any) {
            expected.push(format!(
                "{from}|{g}|{}|{}|{}",
                w.end,
                w.edge_list(0),
                w.edge_list(1)
            ));
        }
    }
    assert_eq!(
        table(&rows(&graph, query), &["a", "g", "b", "e", "f"]),
        sorted(expected),
        "{query}"
    );
    // An edge after it, and one in a second path pattern of the clause.
    let query = "MATCH (a) ((x)-[e]->(m)-[f]->(y)){1,2} (b)-[g]->(c), (c)-[h:likes]->(d) \
                 RETURN a.name AS a, e, f, g, h";
    let mut expected = Vec::new();
    for w in qpp(body, (1, Some(2)), VERTICES, &[], &any) {
        let used = w.edges();
        for (g, from, to, _) in EDGES {
            if *from != w.end || used.iter().any(|u| u == g) {
                continue;
            }
            for (h, hfrom, _, label) in EDGES {
                if hfrom != to || *label != "likes" || h == g || used.iter().any(|u| u == h) {
                    continue;
                }
                expected.push(format!(
                    "{}|{}|{}|{g}|{h}",
                    w.start,
                    w.edge_list(0),
                    w.edge_list(1)
                ));
            }
        }
    }
    assert!(!expected.is_empty());
    assert_eq!(
        table(&rows(&graph, query), &["a", "e", "f", "g", "h"]),
        sorted(expected),
        "{query}"
    );
    // Two patterns in a row: the node between them is shared, and their edges differ.
    let query = "MATCH (a) ((x)-[e]->(y)){1,2} ((u)-[f:knows]->(v)){2} (b) \
                 RETURN a.name AS a, b.name AS b, e, f";
    let mut expected = Vec::new();
    for w in qpp(&[(Dir::Out, &[])], (1, Some(2)), VERTICES, &[], &any) {
        let banned = w.edges();
        let banned: Vec<&str> = banned.iter().map(String::as_str).collect();
        for v in qpp(
            &[(Dir::Out, &["knows"])],
            (2, Some(2)),
            &[&w.end],
            &banned,
            &any,
        ) {
            expected.push(format!(
                "{}|{}|{}|{}",
                w.start,
                v.end,
                w.edge_list(0),
                v.edge_list(0)
            ));
        }
    }
    assert!(!expected.is_empty());
    assert_eq!(
        table(&rows(&graph, query), &["a", "b", "e", "f"]),
        sorted(expected),
        "{query}"
    );
}

#[test]
fn body_conditions_run_in_every_iteration() {
    let graph = cycle();
    let body: &[Hop] = &[(Dir::Out, &[]), (Dir::Out, &[])];
    let cases: Vec<(&str, Keep<'_>)> = vec![
        (
            "MATCH (a) ((x)-[e]->(m)-[f]->(y) WHERE e.name <> 'e1' AND m.name <> 'z'){1,3} (b) \
             RETURN a.name AS a, b.name AS b, e, f",
            &|n, e| e[0] != "e1" && n[1] != "z",
        ),
        // A condition over two elements runs where the later one is bound.
        (
            "MATCH (a) ((x)-[e]->(m)-[f]->(y) WHERE x.name <> y.name){1,} (b) \
             RETURN a.name AS a, b.name AS b, e, f",
            &|n, _| n[0] != n[2],
        ),
        // Inline conditions on body elements, and a condition on no element.
        (
            "MATCH (a) ((x)-[e:knows]->(m {name: 'y'})-[f WHERE f.name <> 'e6']->(y) \
             WHERE $p > 0){1,2} (b) RETURN a.name AS a, b.name AS b, e, f",
            &|n, e| n[1] == "y" && e[1] != "e6",
        ),
    ];
    for (query, keep) in cases {
        let hops: &[Hop] = if query.contains("e:knows") {
            &[(Dir::Out, &["knows"]), (Dir::Out, &[])]
        } else {
            body
        };
        let max = if query.contains("{1,3}") {
            Some(3)
        } else if query.contains("{1,2}") {
            Some(2)
        } else {
            None
        };
        let expected = sorted(
            qpp(hops, (1, max), VERTICES, &[], keep)
                .iter()
                .map(|w| {
                    format!(
                        "{}|{}|{}|{}",
                        w.start,
                        w.end,
                        w.edge_list(0),
                        w.edge_list(1)
                    )
                })
                .collect(),
        );
        assert!(!expected.is_empty(), "{query}");
        let lock = graph.read();
        let got = lock
            .traversal()
            .gql_with(query, [("p", 1_i64)])
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        assert_eq!(table(&got, &["a", "b", "e", "f"]), expected, "{query}");
    }
}

#[test]
fn the_merge_rule_joins_the_outer_nodes() {
    let graph = cycle();
    let body: &[Hop] = &[(Dir::Out, &[]), (Dir::Out, &[])];
    // The node after the pattern is the node before it: every closed walk.
    let query = "MATCH (a) ((x)-[e]->(m)-[f]->(y)){1,} (a) RETURN a.name AS a, e, f";
    let expected = sorted(
        qpp(body, (1, None), VERTICES, &[], &any)
            .iter()
            .filter(|w| w.start == w.end)
            .map(|w| format!("{}|{}|{}", w.start, w.edge_list(0), w.edge_list(1)))
            .collect(),
    );
    assert!(!expected.is_empty());
    assert_eq!(table(&rows(&graph, query), &["a", "e", "f"]), expected);
    // The outer nodes are the first and last body nodes: the lists start and end on them.
    let query = "MATCH (a) ((x)-[e]->(y)){1,3} (b) \
                 RETURN a = head(x) AS first, b = last(y) AS last, count(*) AS n";
    assert_eq!(
        table(&rows(&graph, query), &["first", "last", "n"]),
        [format!(
            "true|true|{}",
            qpp(&[(Dir::Out, &[])], (1, Some(3)), VERTICES, &[], &any).len()
        )]
    );
    // Consecutive iterations share a node: y_i = x_(i+1).
    let query = "MATCH p = ((x)-[e]->(y)){2,3} RETURN \
                 [head(x)] + y = nodes(p) AND x + [last(y)] = nodes(p) AS chained";
    let chained = table(&rows(&graph, query), &["chained"]);
    assert!(!chained.is_empty());
    assert!(chained.iter().all(|c| c == "true"), "{chained:?}");
    // A singleton of an unquantified pattern written again later is a join.
    let query = "MATCH (a) ((x)-[e]->(y) WHERE e.name <> 'e2') (b) MATCH (y)-[f:likes]->(c) \
                 RETURN a.name AS a, e, f";
    assert_eq!(
        table(&rows(&graph, query), &["a", "e", "f"]),
        ["x|e1|e6", "y|e3|e5", "z|e5|e5"]
    );
}

#[test]
fn a_question_mark_binds_conditional_singletons() {
    let graph = cycle();
    let query = "MATCH (a {name: 'x'}) ((x)-[e]->(y))? (b) RETURN b.name AS b, x, e, y";
    assert_eq!(
        table(&rows(&graph, query), &["b", "x", "e", "y"]),
        ["x|null|null|null", "y|x|e1|y", "y|x|e2|y"]
    );
    // A later edge differs from the optional edge when it is there, and passes when it is not
    // (trail against a missing `?` edge).
    let query = "MATCH (a) ((x)-[e]->(y))? (b)-[h]->(c) RETURN a.name AS a, e, h";
    let mut expected = Vec::new();
    for w in qpp(&[(Dir::Out, &[])], (0, Some(1)), VERTICES, &[], &any) {
        let used = w.edges();
        for (h, from, _, _) in EDGES {
            if *from == w.end && !used.iter().any(|u| u == h) {
                let e = used.first().cloned().unwrap_or_else(|| "null".to_string());
                expected.push(format!("{}|{e}|{h}", w.start));
            }
        }
    }
    assert_eq!(
        table(&rows(&graph, query), &["a", "e", "h"]),
        sorted(expected)
    );
    // An outer condition on one runs after the pattern: NULL drops the row.
    let query = "MATCH (a {name: 'x'}) ((x)-[e]->(y))? (b) WHERE y.name = 'y' RETURN e";
    assert_eq!(table(&rows(&graph, query), &["e"]), ["e1", "e2"]);
    // A NULL singleton never joins.
    let query = "MATCH (a {name: 'y'}) ((x)-[e:likes]->(y))? (b) MATCH (y)-[f]->(c) \
                 RETURN e, f";
    assert!(rows(&graph, query).is_empty());
    let query = "MATCH (a {name: 'z'}) ((x)-[e:likes]->(y))? (b) MATCH (y)-[f]->(c) \
                 RETURN e, f";
    // `MATCH (y)-[f]->(c)` is another clause: no trail between `e` and `f`.
    assert_eq!(table(&rows(&graph, query), &["e", "f"]), ["e5|e4", "e5|e5"]);
    // `{0,1}` written out makes group variables (lists).
    let query = "MATCH (a {name: 'x'}) ((x)-[e]->(y)){0,1} (b) RETURN b.name AS b, e";
    assert_eq!(
        table(&rows(&graph, query), &["b", "e"]),
        ["x|[]", "y|[e1]", "y|[e2]"]
    );
}

#[test]
fn quantified_path_patterns_inside_subqueries_and_optional_match() {
    let graph = cycle();
    let body: &[Hop] = &[(Dir::Out, &["knows"]), (Dir::Out, &[])];
    let walks = qpp(body, (1, Some(2)), VERTICES, &[], &any);
    // OPTIONAL MATCH: a row without a match keeps going with NULL lists.
    let query = "MATCH (a) OPTIONAL MATCH (a) ((x)-[e:knows]->(m)-[f]->(y)){1,2} (b) \
                 RETURN a.name AS a, b.name AS b, e, f";
    let mut expected: Vec<String> = walks
        .iter()
        .map(|w| {
            format!(
                "{}|{}|{}|{}",
                w.start,
                w.end,
                w.edge_list(0),
                w.edge_list(1)
            )
        })
        .collect();
    for v in VERTICES {
        if !walks.iter().any(|w| w.start == *v) {
            expected.push(format!("{v}|null|null|null"));
        }
    }
    assert_eq!(
        table(&rows(&graph, query), &["a", "b", "e", "f"]),
        sorted(expected)
    );
    // EXISTS and COUNT { }.
    let query = "MATCH (a) RETURN a.name AS a, \
                 EXISTS { (a) ((x)-[e:knows]->(m)-[f]->(y)){1,2} (b) } AS some, \
                 COUNT { (a) ((x)-[e:knows]->(m)-[f]->(y)){1,2} (b) } AS n";
    let expected = sorted(
        VERTICES
            .iter()
            .map(|v| {
                let n = walks.iter().filter(|w| w.start == *v).count();
                format!("{v}|{}|{n}", n > 0)
            })
            .collect(),
    );
    assert_eq!(table(&rows(&graph, query), &["a", "some", "n"]), expected);
    // A CALL body equals the pattern in the outer MATCH.
    let query = "MATCH (a) CALL (a) { MATCH (a) ((x)-[e:knows]->(m)-[f]->(y)){1,2} (b) \
                 RETURN b, e, f } RETURN a.name AS a, b.name AS b, e, f";
    let expected = sorted(
        walks
            .iter()
            .map(|w| {
                format!(
                    "{}|{}|{}|{}",
                    w.start,
                    w.end,
                    w.edge_list(0),
                    w.edge_list(1)
                )
            })
            .collect(),
    );
    assert_eq!(table(&rows(&graph, query), &["a", "b", "e", "f"]), expected);
    // A pattern predicate and a WHERE EXISTS on the fast path.
    let query = "MATCH (a) WHERE EXISTS { (a) ((x)-[e]->(y)-[f]->(z)){2} } RETURN a.name AS a";
    let expected = sorted(
        VERTICES
            .iter()
            .filter(|v| {
                !qpp(
                    &[(Dir::Out, &[]), (Dir::Out, &[])],
                    (2, Some(2)),
                    &[v],
                    &[],
                    &any,
                )
                .is_empty()
            })
            .map(|v| v.to_string())
            .collect(),
    );
    assert_eq!(table(&rows(&graph, query), &["a"]), expected);
}

/// `body_text` without variables (a pattern predicate introduces none).
fn anonymous(body: &str) -> String {
    let mut text = body.to_string();
    for node in ["x", "y", "m", "n", "o", "q"] {
        text = text.replace(&format!("({node})"), "()");
    }
    for edge in EDGE_NAMES.iter().chain(&["k"]) {
        text = text.replace(&format!("[{edge}"), "[");
    }
    text
}

#[test]
fn pattern_predicates_and_comprehensions_hold_parenthesized_path_patterns() {
    // Stage 2e.T5: a juxtaposed or leading parenthesized path pattern where an expression
    // stands (a pattern predicate, a pattern comprehension) matches what the brute-force walk
    // enumerates, for every body and bound.
    let graph = cycle();
    for hops in BODIES {
        let body = body_text(hops);
        let blank = anonymous(&body);
        for &(quantifier, min, max) in BOUNDS {
            let walks = qpp(hops, (min, max), VERTICES, &[], &any);
            let starts = sorted(
                VERTICES
                    .iter()
                    .filter(|v| walks.iter().any(|w| w.start == **v))
                    .map(|v| v.to_string())
                    .collect(),
            );
            let ends = sorted(
                VERTICES
                    .iter()
                    .filter(|v| walks.iter().any(|w| w.end == **v))
                    .map(|v| v.to_string())
                    .collect(),
            );
            // A node, then the group juxtaposed.
            let query = format!("MATCH (a) WHERE (a) ({blank}){quantifier} RETURN a.name AS a");
            assert_eq!(table(&rows(&graph, &query), &["a"]), starts, "{query}");
            // A leading group, then the node.
            let query = format!("MATCH (b) WHERE ({blank}){quantifier} (b) RETURN b.name AS b");
            assert_eq!(table(&rows(&graph, &query), &["b"]), ends, "{query}");
            // Negated: every other vertex.
            let query = format!("MATCH (a) WHERE NOT (a) ({blank}){quantifier} RETURN a.name AS a");
            let others = sorted(
                VERTICES
                    .iter()
                    .filter(|v| !starts.contains(&v.to_string()))
                    .map(|v| v.to_string())
                    .collect(),
            );
            assert_eq!(table(&rows(&graph, &query), &["a"]), others, "{query}");
            // A comprehension: one item per walk, the lists in text order.
            let query = format!(
                "MATCH (a) FOR r IN [(a) ({body}){quantifier} (b) | {{b: b.name, e: e}}] \
                 RETURN a.name AS a, r.b AS b, r.e AS e"
            );
            let expected = sorted(
                walks
                    .iter()
                    .map(|w| format!("{}|{}|{}", w.start, w.end, w.edge_list(0)))
                    .collect(),
            );
            assert_eq!(
                table(&rows(&graph, &query), &["a", "b", "e"]),
                expected,
                "{query}"
            );
            let query =
                format!("MATCH (a) RETURN a.name AS a, size([({body}){quantifier} (a) | x]) AS n");
            let expected = sorted(
                VERTICES
                    .iter()
                    .map(|v| format!("{v}|{}", walks.iter().filter(|w| w.end == *v).count()))
                    .collect(),
            );
            assert_eq!(
                table(&rows(&graph, &query), &["a", "n"]),
                expected,
                "{query}"
            );
        }
    }
    // `?` in either form: an ISO conditional singleton, NULL after zero iterations.
    let query = "MATCH (a {name: 'w'}) RETURN [(a) ((x)-[e]->(y))? (b) | [b.name, y]] AS l";
    assert_eq!(table(&rows(&graph, query), &["l"]), vec!["[[w null]]"]);
    let query = "MATCH (a {name: 'w'}) WHERE (a) (()-[]->())? RETURN a.name AS a";
    assert_eq!(table(&rows(&graph, query), &["a"]), vec!["w"]);
}

#[test]
fn the_former_not_yet_forms_lower_and_run() {
    let graph = modern();
    for query in [
        "MATCH ((a)-[:knows]->(b)){1,3} RETURN b",
        "MATCH (a) ((x)-->(y))? (b) RETURN x",
        "MATCH (a) ((x)-->(y) WHERE x.age > 1) (b) RETURN b",
        "MATCH (a) OPTIONAL MATCH (a)-->((x)-->(y))+ RETURN a",
        "MATCH (n) WHERE EXISTS { (n) ((x)-->(m)){2} } RETURN n",
        "MATCH (n) WHERE COUNT { MATCH (n) ((x)-->(y))* (m) RETURN m } > 0 RETURN n",
        "MATCH (n) WHERE (n)-->(()-->()){2} RETURN n",
        "CALL { MATCH ((x)-->(y)){2} RETURN x } RETURN x",
        "MATCH (n) RETURN [(n)-->(m) WHERE EXISTS { (m) ((x)-->(y)){2} } | m]",
    ] {
        let lock = graph.read();
        lock.traversal()
            .gql(query)
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
    }
}

#[test]
fn the_plan_records_labels_only_never_a_full_path() {
    let graph = cycle();
    for query in [
        "MATCH (a) ((x)-[e]->(m)-[f]->(y)){1,3} (b) RETURN b",
        "MATCH (a) ((x)-[e]->(m)-[f]->(y)){1,3} (b) RETURN x, m, e",
        "MATCH p = (a) ((x)-[e]->(m)<-[f]-(y)){1,} (b) RETURN p",
        "MATCH (a) ((x)-[e]-(m)-[f]-(y) WHERE x.name <> y.name)* (b {name: 'z'}) RETURN a, e",
        "MATCH (a)-[g]->(c) ((x)-[e]->(y)){2} (b)-[h]->(d) RETURN g, e, h",
        "MATCH (a) OPTIONAL MATCH (a) ((x)-[e]->(m)-[f]->(y)){0,2} (a) RETURN a, e",
        "MATCH (a) WHERE EXISTS { (a) ((x)-[e]->(y)-[f]->(z)){2} } RETURN a",
    ] {
        let plan = profile(&graph, query);
        assert!(plan.contains("repeat("), "{query}\n{plan}");
        assert!(plan.contains("gql:trail("), "{query}\n{plan}");
        assert!(plan.contains("[path: labels("), "{query}\n{plan}");
        assert!(!plan.contains("full"), "{query}\n{plan}");
    }
    // The body: each edge checked for trail before its own label (every iteration of every
    // body edge), the modulators of a quantified edge.
    let plan = profile(
        &graph,
        "MATCH (a) ((x)-[e]->(m)-[f]->(y)){2,3} (b) RETURN e, f",
    );
    assert!(
        plan.contains("repeat(__.out_e().gql:trail(\"e\", \"f\").as(\"e\").in_v()."),
        "{plan}"
    );
    assert!(
        plan.contains(".out_e().gql:trail(\"e\", \"f\").as(\"f\").in_v())"),
        "{plan}"
    );
    assert!(
        plan.contains("times(3).emit(__.loops().is(P.gte(2)))"),
        "{plan}"
    );
    // `?` is `{0,1}`; no quantifier is the body inline, single edges compared by label.
    let plan = profile(&graph, "MATCH (a) ((x)-[e]->(y))? (b)-[h]->(c) RETURN e, h");
    assert!(plan.contains("emit().repeat("), "{plan}");
    assert!(plan.contains("times(1)"), "{plan}");
    assert!(plan.contains("gql:trail(\"e\")"), "{plan}");
    let plan = profile(&graph, "MATCH (a) ((x)-[e]->(m)-[f]->(y)) (b) RETURN e, f");
    assert!(!plan.contains("repeat("), "{plan}");
    assert!(plan.contains("where(P.neq(\"e\"))"), "{plan}");
    // `{0}`: no step, the far node is the near one.
    let plan = profile(&graph, "MATCH (a) ((x)-[e]->(y)){0} (b) RETURN a, b, e");
    assert!(!plan.contains("repeat("), "{plan}");
    assert_eq!(
        table(
            &rows(
                &graph,
                "MATCH (a) ((x)-[e]->(y)){0} (b) RETURN a.name AS a, b.name AS b, e"
            ),
            &["a", "b", "e"]
        ),
        ["w|w|[]", "x|x|[]", "y|y|[]", "z|z|[]"]
    );
}

#[test]
fn reaching_a_limit_is_an_error_naming_the_pattern() {
    let graph = cycle();
    let lock = graph.read();
    let query = "MATCH (a) ((x)-[e]-(m)-[f]-(y)){1,} (b) RETURN count(*) AS n";
    let all = qpp(
        &[(Dir::Any, &[]), (Dir::Any, &[])],
        (1, None),
        VERTICES,
        &[],
        &any,
    )
    .len();
    let n = lock
        .traversal()
        .with("repeat.max_loops", 4)
        .gql(query)
        .select("n")
        .next()
        .unwrap();
    assert_eq!(n, Some(ElementProperty::from(all as i64)));
    let error = lock
        .traversal()
        .with("repeat.max_loops", 1)
        .gql(query)
        .to_list()
        .expect_err("an unbounded quantifier past repeat.max_loops fails");
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::RepeatLimitExceeded { max_loops: 1, .. }
        ),
        "{error}"
    );
    // The body nodes nobody reads carry no label, so the advice shows them empty.
    let help = format_diagnostic(&*error);
    assert!(
        help.contains("the quantified path pattern (()-[e]-()-[f]-()){1,} of the gql() query"),
        "{help}"
    );
    assert!(help.contains("repeats the pattern until"), "{help}");
    assert!(help.contains("(()-[e]-()-[f]-()){1,5}"), "{help}");
    // Inside EXISTS the advice names the subquery.
    let error = lock
        .traversal()
        .with("repeat.max_loops", 1)
        .gql("MATCH (a) WHERE EXISTS { (a) ((x)-[e]-(m)-[f]-(y)){1,} (a) } RETURN a")
        .to_list()
        .expect_err("past repeat.max_loops");
    let help = format_diagnostic(&*error);
    assert!(
        help.contains("quantified path pattern (()-[e]-()-[f]-()){1,} inside an EXISTS"),
        "{help}"
    );
    // Its trails outgrow traversal.max_traversers: an error, never a cut result.
    let error = lock
        .traversal()
        .with("traversal.max_traversers", 10)
        .gql("MATCH (a) ((x)-[e]-(m)-[f]-(y)){1,2} (b) RETURN a, b, e")
        .to_list()
        .expect_err("too many trails");
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::ResourceLimitExceeded { limit, .. } if limit == "traversal.max_traversers"
        ),
        "{error}"
    );
    let help = format_diagnostic(&*error);
    assert!(
        help.contains("Each path a quantified path pattern ((()-[e]-()-[f]-()){1,2})"),
        "{help}"
    );
}

#[test]
fn a_long_body_lowers_and_runs() {
    // 64 body edges (the edge limit of a clause): bounded work per stage.
    let graph = cycle();
    let mut body = String::from("(x0)");
    for i in 0..64 {
        body.push_str(&format!("-[e{i}]-(x{})", i + 1));
    }
    let query = format!("MATCH (a) ({body}){{1,3}} (b) RETURN count(*) AS n");
    let lock = graph.read();
    let n = lock
        .traversal()
        .gql(&query)
        .select("n")
        .next()
        .unwrap_or_else(|e| panic!("{e}"));
    // The fixture has six edges: no trail of 64 edges.
    assert_eq!(n, Some(ElementProperty::from(0_i64)));
    // One edge more is the edge limit.
    let query = format!("MATCH (a) ({body}-[e64]-(z)){{1,3}} (b) RETURN count(*) AS n");
    let error = lock
        .traversal()
        .gql(&query)
        .to_list()
        .expect_err("past the edge limit");
    assert!(format_diagnostic(&*error).contains("54001"), "{error}");
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
    /// `repeat()` of the body without a trail check is the twin of a directed quantified path
    /// pattern.
    #[test]
    fn directed_patterns_match_their_repeat_twins() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let marko = r#"g.V().has("name", "marko")"#;
        let cases: Vec<(&str, String)> = vec![
            (
                "MATCH (a {name: 'marko'}) ((x)-[:knows]->(y)-[:created]->(z)){1,2} (b) \
                 RETURN b.name AS n",
                format!(
                    r#"{marko}.repeat(__.out("knows").out("created")).emit().times(2).project("n").by("name")"#
                ),
            ),
            (
                "MATCH (a:person) ((x)-[e]->(y)){0,2} (b) RETURN a.name AS a, b.name AS b",
                r#"g.V().hasLabel("person").as("a").emit().repeat(__.out()).times(2).project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
            (
                "MATCH (a {name: 'marko'}) ((x)-[e]->(y)){1,2} (b) RETURN b.name AS n, e, y",
                format!(
                    r#"{marko}.repeat(__.outE().as("q").inV().as("w")).emit().times(2).project("n", "e", "y").by("name").by(__.select(Pop.all, "q")).by(__.select(Pop.all, "w"))"#
                ),
            ),
            (
                "MATCH (a) ((x)-[e]->(y) WHERE e.weight > 0.4){1,2} (b) \
                 RETURN a.name AS a, b.name AS b",
                r#"g.V().as("a").repeat(__.outE().has("weight", P.gt(0.4)).inV()).emit().times(2).project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
            (
                "MATCH (a) ((x)-[e]->(y) WHERE y.age > x.age){1,2} (b) \
                 RETURN a.name AS a, b.name AS b",
                r#"g.V().as("a").repeat(__.as("x").out().as("y").where("y", P.gt("x")).by("age")).emit().times(2).project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
            (
                "MATCH (s:software) ((x)<-[:created]-(y)-[:knows]-(z)){1} (b) \
                 RETURN s.name AS a, b.name AS b",
                r#"g.V().hasLabel("software").as("a").in("created").both("knows").project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
            (
                "MATCH (a:person) ((x:person)-[:knows]->(y:person)) (b) \
                 RETURN a.name AS a, b.name AS b",
                r#"g.V().hasLabel("person").as("a").out("knows").hasLabel("person").project("a", "b").by(__.select("a").values("name")).by("name")"#
                    .to_string(),
            ),
            (
                "MATCH (a {name: 'josh'}) ((x)-[e]->(y))? (b) RETURN b.name AS n",
                r#"g.V().has("name", "josh").emit().repeat(__.out()).times(1).project("n").by("name")"#
                    .to_string(),
            ),
            (
                "MATCH p = (a {name: 'marko'}) ((x)-[:knows]->(y)-[:created]->(z)){1} (b) \
                 RETURN p",
                format!(
                    r#"{marko}.outE("knows").inV().outE("created").inV().path().project("p").by()"#
                ),
            ),
        ];
        for (query, twin) in &cases {
            same(&graph, query, twin);
        }
    }
}
