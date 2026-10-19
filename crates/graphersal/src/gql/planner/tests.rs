use super::*;
use crate::gql::ast::Literal;
use crate::gql::bind::bind;
use crate::gql::parser::parse;
use crate::gql::plan::{
    CallStage, CompositePlan, PipelinePlan, PushOp, PushOperand, ResultOp, Stage, classify,
};
use std::collections::BTreeSet;

// ----- helpers -----

/// Facts for tests: label counts, a vertex count, `(label, key)` indexes.
#[derive(Default)]
struct Facts {
    counts: Vec<(&'static str, u64)>,
    total: Option<u64>,
    indexes: Vec<(&'static str, &'static str)>,
}

impl PlannerFacts for Facts {
    fn vertex_label_count(&self, label: &str) -> Option<u64> {
        if self.counts.is_empty() {
            return None;
        }
        Some(
            self.counts
                .iter()
                .find(|(l, _)| *l == label)
                .map_or(0, |(_, n)| *n),
        )
    }
    fn vertex_count(&self) -> Option<u64> {
        self.total
    }
    fn has_vertex_index(&self, label: &str, key: &str) -> bool {
        self.indexes.iter().any(|(l, k)| *l == label && *k == key)
    }
}

fn bound(query: &str) -> BoundQuery {
    let parsed = parse(query).unwrap_or_else(|e| panic!("{query:?} does not parse: {e}"));
    bind(&parsed).unwrap_or_else(|e| panic!("{query:?} does not bind: {e}"))
}

fn plan_with(query: &str, facts: &dyn PlannerFacts) -> (BoundQuery, LogicalPlan) {
    let b = bound(query);
    let p = plan(&b, facts, None);
    (b, p)
}

fn name(b: &BoundQuery, id: BindingId) -> String {
    b.binding(id)
        .and_then(|x| x.name.as_ref())
        .map_or_else(|| format!("_{}", id.0), |n| n.name.clone())
}

/// A compact rendering of the match pipeline: `scan a`, `a-[e]->b`, `join a`, `filter 3`,
/// `trail e2!=e1`, `^a` for a branch, `edge_join`.
fn shape(b: &BoundQuery, p: &LogicalPlan) -> Vec<String> {
    p.ops
        .iter()
        .map(|op| match op {
            Op::Scan(s) => format!("scan {}", name(b, s.binding)),
            Op::Anchor(s) => format!("anchor {}", name(b, s.binding)),
            Op::Lookup(l) => format!("lookup {}", name(b, l.scan.binding)),
            Op::Resume(r) => format!(
                "resume {}{}",
                if r.from_current { "" } else { "^" },
                name(b, r.binding)
            ),
            Op::Expand(e) => {
                let (l, r) = match e.direction {
                    EdgeDirection::Right => ("-", "->"),
                    EdgeDirection::Left => ("<-", "-"),
                    EdgeDirection::Any => ("-", "-"),
                };
                let mut s = format!(
                    "{}{}{l}[{}]{r}{}",
                    if e.from_current { "" } else { "^" },
                    name(b, e.from),
                    name(b, e.edge),
                    name(b, e.to)
                );
                if e.edge_join {
                    s.push_str(" edge_join");
                }
                if !e.trail.is_empty() {
                    let others: Vec<String> = e.trail.iter().map(|&t| name(b, t)).collect();
                    s.push_str(&format!(" trail!={}", others.join(",")));
                }
                s
            }
            Op::Join { binding } => format!("join {}", name(b, *binding)),
            Op::Filter(ids) => format!("filter {ids:?}"),
        })
        .collect()
}

fn labelled(b: &BoundQuery, p: &LogicalPlan) -> Vec<String> {
    p.labelled().into_iter().map(|id| name(b, id)).collect()
}

fn start(b: &BoundQuery, p: &LogicalPlan) -> String {
    match p.ops.first() {
        Some(Op::Scan(s)) => name(b, s.binding),
        other => panic!("no scan first: {other:?}"),
    }
}

fn pushdown_of(query: &str) -> Option<Pushdown> {
    let b = bound(query);
    let c = b.conjuncts.first().expect("one conjunct");
    classify(c)
}

// ----- start choice -----

#[test]
fn start_prefers_an_indexed_equality() {
    let q = "MATCH (a:person {age: 29})-[:knows]->(b:person {name: 'josh'}) RETURN *";
    let facts = Facts {
        indexes: vec![("person", "name")],
        ..Facts::default()
    };
    let (b, p) = plan_with(q, &facts);
    assert_eq!(start(&b, &p), "b");
    assert_eq!(shape(&b, &p), ["scan b", "b<-[_1]-a"]);
    // Without the index both are label + equality: the first node wins.
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(start(&b, &p), "a");
    // An index on a key the node only compares by range does not count.
    let q = "MATCH (a:person {age: 29})-->(b:person) WHERE b.name > 'j' RETURN *";
    let (b, p) = plan_with(q, &facts);
    assert_eq!(start(&b, &p), "a");
    // Every scan label needs the index (`A|B`).
    let q = "MATCH (a:person)-->(b:person|dog {name: 'x'}) RETURN *";
    let (b, p) = plan_with(q, &facts);
    // b is level 1 (label + equality), still before a (label only).
    assert_eq!(start(&b, &p), "b");
    let facts2 = Facts {
        indexes: vec![("person", "name"), ("dog", "name")],
        counts: vec![("person", 1), ("dog", 1000)],
        ..Facts::default()
    };
    let q = "MATCH (a:person {name: 'x'})-->(b:person|dog {name: 'x'}) RETURN *";
    let (b, p) = plan_with(q, &facts2);
    assert_eq!(start(&b, &p), "a", "both indexed: the smaller estimate");
}

#[test]
fn start_prefers_label_and_property_over_label() {
    let q = "MATCH (a:person)-[:created]->(s:software {name: 'lop'}) RETURN *";
    let facts = Facts {
        counts: vec![("person", 1), ("software", 1000)],
        ..Facts::default()
    };
    let (b, p) = plan_with(q, &facts);
    assert_eq!(start(&b, &p), "s");
    // A WHERE equality counts too, an IN as well; a residual conjunct does not.
    let q = "MATCH (a:person)-->(s:software) WHERE s.name IN ['lop'] RETURN *";
    let (b, p) = plan_with(q, &facts);
    assert_eq!(start(&b, &p), "s");
    let q = "MATCH (a:person)-->(s:software) WHERE s.name <> 'lop' RETURN *";
    let (b, p) = plan_with(q, &facts);
    assert_eq!(start(&b, &p), "a");
}

#[test]
fn start_prefers_the_smallest_label_count() {
    let q = "MATCH (a:person)-[:created]->(s:software) RETURN *";
    let facts = Facts {
        counts: vec![("person", 4), ("software", 2)],
        ..Facts::default()
    };
    let (b, p) = plan_with(q, &facts);
    assert_eq!(start(&b, &p), "s");
    assert_eq!(shape(&b, &p), ["scan s", "s<-[_1]-a"]);
    // Unknown counts: the first node.
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(start(&b, &p), "a");
    // `A|B` sums, `A&B` takes the smaller factor.
    let q = "MATCH (a:person|software)-->(b:person&software) RETURN *";
    let (b, p) = plan_with(q, &facts);
    assert_eq!(start(&b, &p), "b");
}

#[test]
fn start_falls_back_to_the_first_node() {
    let (b, p) = plan_with("MATCH (a)-->(b)<--(c) RETURN *", &NoFacts);
    assert_eq!(start(&b, &p), "a");
    // A label beats no label even when it comes later; a negated label is no scan label.
    let (b, p) = plan_with("MATCH (a:!x)-->(b {k: 1})-->(c:y) RETURN *", &NoFacts);
    assert_eq!(start(&b, &p), "c");
}

// ----- walk order -----

#[test]
fn walk_prefers_far_nodes_with_filters_then_continuing() {
    let q = "MATCH (a:x {k: 1})-->(b)-->(c), (b)-->(d:y {k: 2}) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        [
            "scan a",
            "a-[_1]->b",
            "b-[_5]->d trail!=_1",
            "^b-[_3]->c trail!=_1,_5"
        ]
    );
    // b is the origin of a branch, so it is labelled (and read by RETURN * anyway).
    assert!(p.labelling[b.lookup("b").unwrap().0].branch);
    // Labels only rank below own conjuncts, above nothing.
    let q = "MATCH (a:x)-->(b), (a)-->(c:y), (a)-->(d {k: 1}) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        [
            "scan a",
            "a-[_5]->d",
            "^a-[_3]->c trail!=_5",
            "^a-[_1]->b trail!=_5,_3"
        ]
    );
}

#[test]
fn walk_reverses_directions_as_seen_from_the_walk() {
    let q = "MATCH (a)-[e]->(b)<-[f]-(c:z) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan c", "c-[f]->b", "b<-[e]-a trail!=f"]);
    let q = "MATCH (a)-[e]-(b:z) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan b", "b-[e]-a"]);
}

// ----- joins and cycles -----

#[test]
fn a_triangle_closes_with_one_join() {
    let q = "MATCH (a:p {k: 1})-[:t]->(b)-[:t]->(c)-[:t]->(a) RETURN c.k AS k";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        [
            "scan a",
            "a-[_1]->b",
            "b-[_3]->c trail!=_1",
            "c-[_5]->a trail!=_1,_3",
            "join a"
        ]
    );
    // a: join target; c: read; the anonymous edges: trail targets; b: nothing.
    assert_eq!(labelled(&b, &p), ["a", "_1", "_3", "c"]);
    let Some(Op::Expand(last)) = p.ops.get(3) else {
        panic!()
    };
    assert!(last.to_bound && last.to_filters.is_empty());
}

#[test]
fn a_cycle_edge_is_walked_as_soon_as_both_ends_are_bound() {
    // a-b, b-c, a-c: after a and b, the edge a-c is not closing yet; after c it is, walked
    // from the current element c, before c-d.
    let q = "MATCH (a:p)-[:x]->(b), (b)-[:y]->(c), (c)-[:z]->(d), (a)-[:w]->(c) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        [
            "scan a",
            "a-[_1]->b",
            "b-[_3]->c",
            "c<-[_7]-a",
            "join a",
            "^c-[_5]->d"
        ]
    );
}

#[test]
fn a_repeated_edge_across_clauses_joins_on_the_edge() {
    // c and d reach a and b only through the edge e: a second scan, then the edge check.
    let q = "MATCH (a:p)-[e:k]->(b) MATCH (c)-[e]->(d) RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        ["scan a", "a-[e]->b", "scan c", "c-[e]->d edge_join"]
    );
    assert_eq!(labelled(&b, &p), ["a", "e"]);
    // A node repeated in a later MATCH joins like a cycle; no trail between clauses.
    let q = "MATCH (a:p)-[e1]->(b) MATCH (b)-[e2]->(c), (c)-[e3]->(a) RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        [
            "scan a",
            "a-[e1]->b",
            "b-[e2]->c",
            "c-[e3]->a trail!=e2",
            "join a"
        ]
    );
    assert_eq!(labelled(&b, &p), ["a", "e2"]);
}

// ----- trail -----

#[test]
fn trail_checks_only_edges_whose_labels_may_overlap() {
    let q = "MATCH (a:p)-[e1]-(b)-[e2]-(c) RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "a-[e1]-b", "b-[e2]-c trail!=e1"]);
    assert_eq!(labelled(&b, &p), ["a", "e1"]);
    // Disjoint label sets: no check, no label.
    let q = "MATCH (a:p)-[e1:knows]->(b)-[e2:created]->(c) RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "a-[e1]->b", "b-[e2]->c"]);
    assert_eq!(labelled(&b, &p), ["a"]);
    // `A|B` vs `B`, a wildcard, a negation: may overlap.
    for (l1, l2) in [
        ("knows|likes", "likes"),
        ("%", "knows"),
        ("!knows", "knows"),
    ] {
        let q = format!("MATCH (a:p)-[e1:{l1}]->(b)-[e2:{l2}]->(c) RETURN a");
        let (b, p) = plan_with(&q, &NoFacts);
        assert_eq!(shape(&b, &p)[2], "b-[e2]->c trail!=e1", "{l1} vs {l2}");
    }
    // `A&B` (A != B) can hold for no edge: no check.
    let q = "MATCH (a:p)-[e1:x&y]->(b)-[e2]->(c) RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p)[2], "b-[e2]->c");
    // Comma patterns of one MATCH share the trail scope, even when disconnected.
    let q = "MATCH (a:p)-[e1]->(b), (c:q)-[e2]->(d) RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        ["scan a", "a-[e1]->b", "scan c", "c-[e2]->d trail!=e1"]
    );
}

// ----- filters -----

#[test]
fn filters_are_placed_as_early_as_possible() {
    let q = "MATCH (a:p)-->(b)-->(c) WHERE a.x = c.x AND a.y = b.y AND $flag RETURN c";
    let (b, p) = plan_with(q, &NoFacts);
    // conjuncts: 0 a.x = c.x, 1 a.y = b.y, 2 $flag
    assert_eq!(
        shape(&b, &p),
        [
            "scan a",
            "filter [2]",
            "a-[_1]->b",
            "filter [1]",
            "b-[_3]->c trail!=_1",
            "filter [0]"
        ]
    );
}

#[test]
fn element_filters_split_pushed_and_residual() {
    let q = "MATCH (a:person {name: 'x'})-[e:k {w: 1}]->(b) \
             WHERE a.age > 3 AND a.age <> 4 AND a.nick STARTS WITH 'm' AND e.w IS NULL \
             AND b.age IS NOT NULL RETURN a";
    let facts = Facts {
        indexes: vec![("person", "age")],
        ..Facts::default()
    };
    let (b, p) = plan_with(q, &facts);
    let Some(Op::Scan(scan)) = p.ops.first() else {
        panic!()
    };
    // indexed range first, then the equality, then the text form; `<>` stays residual.
    let text: Vec<&str> = scan
        .filters
        .pushed
        .iter()
        .map(|&c| b.conjuncts[c].span)
        .map(|s| &q[s.start..s.end])
        .collect();
    assert_eq!(text, ["a.age > 3", "name: 'x'", "a.nick STARTS WITH 'm'"]);
    assert_eq!(scan.filters.residual.len(), 1);
    assert_eq!(
        scan.scan_labels.as_deref(),
        Some(&["person".to_owned()][..])
    );
    let Some(Op::Expand(x)) = p.ops.get(1) else {
        panic!()
    };
    assert_eq!(x.edge_filters.pushed.len(), 1);
    assert_eq!(x.edge_filters.residual.len(), 1);
    assert_eq!(x.to_filters.pushed.len(), 1);
    assert!(x.to_filters.residual.is_empty());
}

// ----- components -----

#[test]
fn components_are_emitted_best_start_first() {
    let q = "MATCH (a:person), (s:software {name: 'lop'}) WHERE a.x = s.x RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan s", "scan a", "filter [1]"]);
    let q = "MATCH (a)-->(b), (c:x)-->(d) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        ["scan c", "c-[_4]->d", "scan a", "a-[_1]->b trail!=_4"]
    );
    // Components joined by a later MATCH are one component.
    let q = "MATCH (a:x), (b:y) MATCH (a)-->(b) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "a-[_2]->b"]);
}

// ----- labelling -----

#[test]
fn only_read_join_trail_and_branch_targets_are_labelled() {
    let q = "MATCH (a:person)-[:knows]->(b) RETURN b.name";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(labelled(&b, &p), ["b"]);
    // An own filter is no read.
    let q = "MATCH (a:person)-[:knows]->(b) WHERE b.age > 3 AND a.name = 'x' RETURN b.name";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(labelled(&b, &p), ["b"]);
    let q = "MATCH (a:person)-[:knows]->(b) WHERE b.age > a.age RETURN 1 AS one";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(labelled(&b, &p), ["a", "b"]);
}

#[test]
fn the_plan_is_deterministic() {
    let q = "MATCH (a)-[e1]-(b)-[e2]-(c), (c)-[e3]-(a), (d)-->(e) WHERE a.k = d.k \
             RETURN DISTINCT a, b ORDER BY a SKIP 1 LIMIT 2";
    let facts = Facts {
        counts: vec![("x", 3)],
        total: Some(10),
        indexes: vec![],
    };
    let (b, p) = plan_with(q, &facts);
    for _ in 0..8 {
        assert_eq!(plan(&b, &facts, None), p);
    }
    assert_eq!(
        p.result,
        [
            ResultOp::Project,
            ResultOp::Distinct,
            ResultOp::Order,
            ResultOp::Offset(crate::gql::bind::Count::Literal(1)),
            ResultOp::Limit(crate::gql::bind::Count::Literal(2)),
        ]
    );
}

#[test]
fn the_anchor_starts_its_component() {
    let b = bound("MATCH (a:p {k: 1})-->(b), (c:q)-->(d) RETURN *");
    let anchor = b.lookup("d");
    let p = plan(&b, &NoFacts, anchor);
    assert_eq!(p.anchor, anchor);
    assert!(matches!(&p.ops[0], Op::Anchor(s) if Some(s.binding) == anchor));
    assert_eq!(
        shape(&b, &p),
        ["anchor d", "d<-[_4]-c", "scan a", "a-[_1]->b trail!=_4"]
    );
    // An edge is no anchor.
    let p = plan(&b, &NoFacts, Some(BindingId(1)));
    assert_eq!(p.anchor, None);
    assert_eq!(crate::gql::plan::ANCHOR_VARIABLE, "");
    assert!(parse("MATCH (``) RETURN *").is_err());
}

// ----- push-down classification (Phase 0 Q5) -----

#[test]
fn pushdown_safe_forms() {
    let cases: [(&str, PushOp, PushOperand); 14] = [
        (
            "n.v = 30",
            PushOp::Eq,
            PushOperand::Value(Literal::Integer(30)),
        ),
        (
            "n.v = '30'",
            PushOp::Eq,
            PushOperand::Value(Literal::String("30".into())),
        ),
        (
            "n.v < 30",
            PushOp::Lt,
            PushOperand::Value(Literal::Integer(30)),
        ),
        (
            "n.v <= 3.5",
            PushOp::Le,
            PushOperand::Value(Literal::Float(3.5)),
        ),
        (
            "n.v > -10",
            PushOp::Gt,
            PushOperand::Value(Literal::Integer(-10)),
        ),
        (
            "n.v >= false",
            PushOp::Ge,
            PushOperand::Value(Literal::Bool(false)),
        ),
        (
            "30 < n.v",
            PushOp::Gt,
            PushOperand::Value(Literal::Integer(30)),
        ),
        (
            "n.v IN [30, NULL, 'abc']",
            PushOp::In,
            PushOperand::List(vec![Literal::Integer(30), Literal::String("abc".into())]),
        ),
        ("n.v IN []", PushOp::In, PushOperand::List(vec![])),
        (
            "n.v STARTS WITH 'a'",
            PushOp::StartsWith,
            PushOperand::Value(Literal::String("a".into())),
        ),
        (
            "n.v ENDS WITH 'c'",
            PushOp::EndsWith,
            PushOperand::Value(Literal::String("c".into())),
        ),
        (
            "n.v CONTAINS ''",
            PushOp::Contains,
            PushOperand::Value(Literal::String(String::new())),
        ),
        ("n.v IS NOT NULL", PushOp::NotNull, PushOperand::None),
        (
            "n.v CONTAINS $s",
            PushOp::Contains,
            PushOperand::Parameter {
                name: "s".into(),
                span: crate::gql::lexer::Span {
                    start: 0,
                    end: 0,
                    line: 0,
                    column: 0,
                },
            },
        ),
    ];
    for (cond, op, operand) in cases {
        let q = format!("MATCH (n) WHERE {cond} RETURN n");
        let p = pushdown_of(&q).unwrap_or_else(|| panic!("{cond} not pushed"));
        assert_eq!(p.key, "v", "{cond}");
        assert_eq!(p.op, op, "{cond}");
        match (&p.operand, &operand) {
            (PushOperand::Parameter { name: a, .. }, PushOperand::Parameter { name: b, .. }) => {
                assert_eq!(a, b)
            }
            _ => assert_eq!(p.operand, operand, "{cond}"),
        }
    }
    // Inline properties are `=`; a parameter operand too.
    let p = pushdown_of("MATCH (n {v: $x}) RETURN n").unwrap();
    assert_eq!(p.op, PushOp::Eq);
    assert!(matches!(p.operand, PushOperand::Parameter { ref name, .. } if name == "x"));
    let p = pushdown_of("MATCH (n)-[e {w: 0.5}]->(m) RETURN n").unwrap();
    assert_eq!(p.key, "w");
}

#[test]
fn pushdown_refuses_the_unsafe_forms() {
    for cond in [
        "n.v <> 30",
        "n.v IS NULL",
        "n.v = NULL",
        "n.v > NULL",
        "NULL = n.v",
        "n.v = [30]",
        "n.v IN [[30]]",
        "n.v IN [n.w]",
        "n.v STARTS WITH 3",
        "n.v CONTAINS NULL",
        "n.v = n.w",
        "n.v = 1 + 2",
        "NOT n.v = 30",
        "n.v = 30 OR n.v = 31",
        "n.v + 1 = 30",
        "30 IN n.v",
        "'a' STARTS WITH n.v",
        "n = n",
        "n.v = {a: 1}",
    ] {
        let q = format!("MATCH (n) WHERE {cond} RETURN n");
        assert_eq!(pushdown_of(&q), None, "{cond} must stay in gql:filter");
    }
    assert_eq!(pushdown_of("MATCH (n {v: null}) RETURN n"), None);
    // Two bindings: never pushed.
    assert_eq!(
        pushdown_of("MATCH (n)-->(m) WHERE n.v = m.v RETURN n"),
        None
    );
}

// ----- the storage adapter -----

#[test]
fn storage_facts_read_statistics_and_untransformed_vertex_indexes() {
    use crate::graph::{PropertyIndexDefinition, TraversalGraph};
    let mut graph = TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .create_index(
            "person_name",
            PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
        )
        .unwrap();
    graph
        .traversal_mut()
        .create_index(
            "knows_weight",
            PropertyIndexDefinition::new(ElementKind::Edge, "knows", "weight"),
        )
        .unwrap();
    let facts = StorageFacts::from_storage(&graph);
    assert_eq!(facts.vertex_label_count("person"), Some(4));
    assert_eq!(facts.vertex_label_count("software"), Some(2));
    assert_eq!(facts.vertex_label_count("nothing"), Some(0));
    assert_eq!(facts.vertex_count(), Some(6));
    assert!(facts.has_vertex_index("person", "name"));
    assert!(!facts.has_vertex_index("person", "age"));
    assert!(!facts.has_vertex_index("knows", "weight"));
    let (b, p) = plan_with(
        "MATCH (a:person {age: 29})-->(s:software {name: 'lop'})<--(c:person {name: 'josh'}) \
         RETURN *",
        &facts,
    );
    assert_eq!(start(&b, &p), "c");
}

// ----- stage 1b: segments, quantified edges, aggregation, labelling reasons -----

/// The plan rendering of `ops` (as [`shape`]).
fn ops_shape(b: &BoundQuery, p: &LogicalPlan, ops: &[Op]) -> Vec<String> {
    let mut only = p.clone();
    only.body.first.first.ops = ops.to_vec();
    shape(b, &only)
}

/// One line per later stage: `match: ..`, `optional: .. witness w`, `with [..] filters [..]`
/// (`labelled` when the row is labelled).
fn stages(b: &BoundQuery, p: &LogicalPlan) -> Vec<String> {
    p.stages
        .iter()
        .map(|stage| match stage {
            Stage::Match { ops, .. } => format!("match: {}", ops_shape(b, p, ops).join("; ")),
            Stage::Optional { ops, witness, .. } => format!(
                "optional: {}{}",
                ops_shape(b, p, ops).join("; "),
                witness.map_or(String::new(), |w| format!(" witness {}", name(b, w)))
            ),
            Stage::Materialize(m) => format!(
                "with {:?} filters {:?}{}",
                m.result,
                m.filters,
                if m.labelled { " labelled" } else { "" }
            ),
            Stage::For(f) => format!("for{}", if f.labelled { " labelled" } else { "" }),
            Stage::Call(c) => format!("call{}", if c.labelled { " labelled" } else { "" }),
            Stage::Rows(r) => format!("rows{}", if r.labelled { " labelled" } else { "" }),
        })
        .collect()
}

/// The bindings named `n`, in id order (a pattern binding and the WITH column bindings).
fn named(b: &BoundQuery, n: &str) -> Vec<BindingId> {
    b.bindings
        .iter()
        .filter(|x| x.name.as_ref().is_some_and(|i| i.name == n))
        .map(|x| x.id)
        .collect()
}

fn reasons(p: &LogicalPlan, id: BindingId) -> Labelling {
    p.labelling[id.0]
}

fn only_1a_reasons(l: &Labelling) -> bool {
    !l.resume && !l.optional && !l.group && !l.path
}

#[test]
fn a_1a_query_is_one_segment_without_stages() {
    for q in [
        "MATCH (a:person)-[:knows]->(b), (c)-->(a) WHERE a.age > b.age RETURN DISTINCT a, c \
         ORDER BY a.name SKIP 1 LIMIT 2",
        "MATCH (a)-[e]-(b)-[f]-(a) RETURN e",
        "MATCH (a) MATCH (a)-->(b) RETURN b.name AS name",
    ] {
        let (_, p) = plan_with(q, &NoFacts);
        assert!(p.stages.is_empty(), "{q}");
        assert!(p.labelling.iter().all(only_1a_reasons), "{q}");
        assert_eq!(p.result.first(), Some(&ResultOp::Project), "{q}");
        for op in &p.ops {
            if let Op::Expand(e) = op {
                assert_eq!(e.quantifier, None, "{q}");
            }
        }
    }
    // A plain edge walked from its written target is reversed too.
    let q = "MATCH (a)-[e]->(b:person {name: 'x'}) RETURN e";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan b", "b<-[e]-a"]);
    assert!(matches!(&p.ops[1], Op::Expand(e) if e.reversed));
    let (_, p) = plan_with("MATCH (a:person {name: 'x'})-[e]->(b) RETURN e", &NoFacts);
    assert!(matches!(&p.ops[1], Op::Expand(e) if !e.reversed));
}

#[test]
fn an_optional_match_continues_from_the_current_element() {
    let q = "MATCH (a:person) OPTIONAL MATCH (a)-[:knows]->(b) RETURN a.name, b.name";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a"]);
    assert_eq!(stages(&b, &p), ["optional: resume a; a-[_1]->b witness b"]);
    let (a, bb) = (named(&b, "a")[0], named(&b, "b")[0]);
    // Both are read after the optional part: through their labels, never the current element.
    assert!(reasons(&p, a).optional && !reasons(&p, a).resume);
    assert!(reasons(&p, bb).optional);
    assert_eq!(labelled(&b, &p), ["a", "b"]);
    // The part's own conditions stay inside it, also one on the outer binding.
    let q = "MATCH (a:person) OPTIONAL MATCH (a)-[:knows]->(b:person) WHERE a.age < b.age \
             AND a.name <> 'x' RETURN b";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        ["optional: filter [1]; resume a; a-[_1]->b; filter [0] witness b"]
    );
}

#[test]
fn an_optional_match_away_from_the_current_element_resumes_with_a_label() {
    let q = "MATCH (a:person)-[:created]->(s) OPTIONAL MATCH (a)-[:knows]->(c) RETURN c.name";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "a-[_1]->s"]);
    assert_eq!(stages(&b, &p), ["optional: resume ^a; a-[_3]->c witness c"]);
    let a = named(&b, "a")[0];
    assert!(reasons(&p, a).resume && !reasons(&p, a).optional);
    assert_eq!(labelled(&b, &p), ["a", "c"]);
    // Two components of one part: the current element's first, the other one by a label.
    let q = "MATCH (a)-->(s) OPTIONAL MATCH (a)-->(x), (s)-->(y) RETURN x, y";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        ["optional: resume s; s-[_5]->y; resume ^a; a-[_3]->x trail!=_5 witness x"]
    );
}

#[test]
fn a_leading_optional_match_has_no_first_segment() {
    let (b, p) = plan_with("OPTIONAL MATCH (a:person)-->(b) RETURN a, b", &NoFacts);
    assert!(p.ops.is_empty());
    assert_eq!(stages(&b, &p), ["optional: scan a; a-[_1]->b witness a"]);
    assert_eq!(labelled(&b, &p), ["a", "b"]);
}

#[test]
fn a_plain_match_after_an_optional_part_reads_through_labels() {
    let q = "MATCH (a)-->(b) OPTIONAL MATCH (b)-->(c) MATCH (a)-->(d) RETURN a, c, d";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "a-[_1]->b"]);
    assert_eq!(
        stages(&b, &p),
        [
            "optional: resume b; b-[_3]->c witness c",
            "match: resume ^a; a-[_5]->d",
        ]
    );
    let a = named(&b, "a")[0];
    assert!(reasons(&p, a).resume && reasons(&p, a).optional);
    // `d` is bound after the optional part ended: the stage 1a rules hold for it.
    assert!(!reasons(&p, named(&b, "d")[0]).optional);
    // b is not read after the part (only resumed from inside it, as the current element).
    assert!(!p.must_label(named(&b, "b")[0]));
}

#[test]
fn a_materializing_with_resumes_from_its_column() {
    let q = "MATCH (a:person) WITH a, count(*) AS n MATCH (a)-->(c) RETURN c";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a"]);
    assert_eq!(
        stages(&b, &p),
        ["with [Aggregate] filters []", "match: resume ^a; a-[_3]->c"]
    );
    let column = named(&b, "a")[1];
    assert!(b.binding(column).is_some_and(|x| x.column.is_some()));
    // A column binding is read from its row, never labelled itself.
    assert!(!p.must_label(column));
    // Reading a column after the row was left labels the row.
    let q = "MATCH (a:person) WITH a, count(*) AS n MATCH (a)-->(c) RETURN a, n, c";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        [
            "with [Aggregate] filters [] labelled",
            "match: resume ^a; a-[_3]->c"
        ]
    );
    // A join back to a column reads it after the row was left too.
    let q = "MATCH (a:person) WITH a, count(*) AS n MATCH (a)-->(c)-->(a) RETURN c";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        [
            "with [Aggregate] filters [] labelled",
            "match: resume ^a; a-[_3]->c; c-[_5]->a trail!=_3; join a",
        ]
    );
}

#[test]
fn a_materializing_with_read_on_its_row_needs_no_row_label() {
    let q = "MATCH (a) WITH a, count(*) AS n WITH a, n ORDER BY n DESC LIMIT 1 WHERE n > 0 \
             RETURN a, n";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        [
            "with [Aggregate] filters []",
            "with [Project, Order, Limit(Literal(1))] filters [0]",
        ]
    );
    // DISTINCT, OFFSET and a FILTER after the cut (a filter-only stage on the row).
    let q = "MATCH (a) WITH DISTINCT a.name AS name SKIP 1 FILTER name <> 'x' RETURN name";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        [
            "with [Project, Distinct, Offset(Literal(1))] filters []",
            "match: filter [0]",
        ]
    );
    // An OPTIONAL MATCH after it: the row is left inside the part.
    let q = "MATCH (a) WITH a ORDER BY a.age LIMIT 2 OPTIONAL MATCH (a)-->(b) RETURN a, b";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        [
            "with [Project, Order, Limit(Literal(2))] filters [] labelled",
            "optional: resume ^a; a-[_2]->b witness b",
        ]
    );
}

#[test]
fn a_name_bound_again_after_with_is_a_new_node() {
    let q = "MATCH (a)-->(b) WITH a, count(b) AS n MATCH (b)-->(a) RETURN b";
    let (b, p) = plan_with(q, &NoFacts);
    let bs = named(&b, "b");
    assert_eq!(bs.len(), 2);
    assert_ne!(
        b.binding(bs[1]).and_then(|x| x.key.clone()).as_deref(),
        Some("b")
    );
    assert_eq!(
        stages(&b, &p),
        ["with [Aggregate] filters []", "match: resume ^a; a<-[_6]-b"]
    );
}

#[test]
fn let_filter_and_a_projecting_with_plan_like_stage_1a() {
    let pairs = [
        (
            "MATCH (a:person) LET x = a.age FILTER x > 30 RETURN x",
            "MATCH (a:person) WHERE a.age > 30 RETURN a.age AS x",
        ),
        (
            "MATCH (a:person) WITH a WHERE a.age > 30 MATCH (a)-->(b) RETURN b",
            "MATCH (a:person) WHERE a.age > 30 MATCH (a)-->(b) RETURN b",
        ),
    ];
    for (q, twin) in pairs {
        let (b, p) = plan_with(q, &NoFacts);
        let (tb, tp) = plan_with(twin, &NoFacts);
        assert!(p.stages.is_empty(), "{q}");
        assert_eq!(shape(&b, &p), shape(&tb, &tp), "{q}");
        assert_eq!(p.ops, tp.ops, "{q}");
    }
}

#[test]
fn aggregation_is_a_result_operation() {
    let q = "MATCH (a:person) RETURN a.name AS name, count(*) AS n ORDER BY n DESC LIMIT 3";
    let (_, p) = plan_with(q, &NoFacts);
    assert_eq!(
        p.result,
        [
            ResultOp::Aggregate,
            ResultOp::Order,
            ResultOp::Limit(crate::gql::bind::Count::Literal(3)),
        ]
    );
    let (_, p) = plan_with("MATCH (a) RETURN DISTINCT count(a) + 1 AS n", &NoFacts);
    assert_eq!(p.result, [ResultOp::Aggregate, ResultOp::Distinct]);
}

#[test]
fn quantified_edges_walk_after_plain_ones() {
    // Both edges continue from b: the plain one first, though written last.
    let q = "MATCH (b:person {name: 'x'})-[e:knows]->{1,2}(a), (b)-[:created]->(c) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan b", "b-[_3]->c", "^b-[e]->a"]);
    let Some(Op::Expand(quantified)) = p.ops.get(2) else {
        panic!("an expansion");
    };
    let quantifier = quantified.quantifier.expect("quantified");
    assert_eq!((quantifier.min, quantifier.max), (1, Some(2)));
    assert!(!quantified.reversed);
    // Walked from its written target: reversed.
    let q = "MATCH (a)-[e:knows]->{1,3}(b:person {name: 'x'})-[:created]->(c) RETURN a, b";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan b", "b-[_3]->c", "^b<-[e]-a"]);
    let Some(Op::Expand(quantified)) = p.ops.get(2) else {
        panic!("an expansion");
    };
    assert!(quantified.reversed && quantified.quantifier.is_some());
    // A quantified edge back to its start closes with a join.
    let (b, p) = plan_with("MATCH (a)-[:knows]->{1,3}(a) RETURN a", &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "a-[_1]->a", "join a"]);
}

#[test]
fn trail_lists_hold_group_variables() {
    let q = "MATCH (a)-[e1]->(b)-[e2]->{1,2}(c)-[e3]->(d) RETURN *";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        shape(&b, &p),
        [
            "scan a",
            "a-[e1]->b",
            "b-[e2]->c trail!=e1",
            "c-[e3]->d trail!=e1,e2"
        ]
    );
    let e2 = named(&b, "e2")[0];
    assert!(reasons(&p, e2).group && reasons(&p, e2).trail);
    // Disjoint label sets still need no check, a group variable included.
    let q = "MATCH (a)-[:knows]->{1,2}(b)-[:created]->(c) RETURN c";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "a-[_1]->b", "b-[_3]->c"]);
    // Every group variable is labelled, read or not.
    assert!(reasons(&p, BindingId(1)).group);
    assert!(!reasons(&p, BindingId(3)).needed());
}

#[test]
fn read_path_variables_label_their_elements() {
    let q = "MATCH p = (a)-[:knows]->{1,3}(b)-->(c) RETURN p";
    let (b, p) = plan_with(q, &NoFacts);
    for n in ["a", "b", "c"] {
        assert!(reasons(&p, named(&b, n)[0]).path, "{n}");
    }
    assert!(reasons(&p, BindingId(2)).path && reasons(&p, BindingId(4)).path);
    // The path binding itself never needs a label of its own reasons but `read`.
    let path = named(&b, "p")[0];
    assert!(only_1a_reasons(&reasons(&p, path)));
    // An unread path variable labels nothing.
    let (_, p) = plan_with("MATCH p = (a)-->(b) RETURN a.name", &NoFacts);
    assert!(p.labelling.iter().all(|l| !l.path));
}

#[test]
fn a_group_variable_read_after_its_optional_part_labels_the_witness() {
    let q = "MATCH (a) OPTIONAL MATCH (a)-[e]->{0,2}(b) RETURN e";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(stages(&b, &p), ["optional: resume a; a-[e]->b witness b"]);
    let witness = named(&b, "b")[0];
    assert!(reasons(&p, witness).optional);
    assert_eq!(labelled(&b, &p), ["e", "b"]);
    // Not read after the part: the witness stays unlabelled.
    let (b, p) = plan_with(
        "MATCH (a) OPTIONAL MATCH (a)-[e]->{0,2}(b) RETURN a",
        &NoFacts,
    );
    assert!(!p.must_label(named(&b, "b")[0]));
    // No new node in the part: no witness.
    let (b, p) = plan_with(
        "MATCH (a) OPTIONAL MATCH (a)-[e]->{0,2}(a) RETURN e",
        &NoFacts,
    );
    assert_eq!(stages(&b, &p), ["optional: resume a; a-[e]->a; join a"]);
}

#[test]
fn a_stage_1b_plan_is_deterministic() {
    let q = "MATCH (a:person)-[e:knows]->{1,3}(b) OPTIONAL MATCH (b)-->(c), (a)-[:created]->(d) \
             WITH a, count(c) AS n, collect_list(d) AS ds ORDER BY n LIMIT 5 \
             MATCH p = (a)-->(x)<-[:knows*]-(y) WHERE x.age > n RETURN p, n";
    let facts = Facts {
        counts: vec![("person", 4)],
        total: Some(6),
        indexes: vec![],
    };
    let (b, p) = plan_with(q, &facts);
    for _ in 0..8 {
        assert_eq!(plan(&b, &facts, None), p);
    }
    assert_eq!(p.stages.len(), 3);
}

// ----- stage 2a: FOR / UNWIND and the unit row -----

#[test]
fn for_is_a_stage_whose_row_is_labelled_only_when_read_after_leaving_it() {
    // Read on its own row: no row label, no first segment (the unit row).
    let (b, p) = plan_with("FOR x IN [1, 2] RETURN x", &NoFacts);
    assert!(p.ops.is_empty());
    assert_eq!(stages(&b, &p), ["for"]);
    assert!(p.labelled().is_empty());
    // Read after a later MATCH left it.
    let (b, p) = plan_with("FOR x IN [1] MATCH (a:person) RETURN a, x", &NoFacts);
    assert_eq!(stages(&b, &p), ["for labelled", "match: scan a"]);
    // A FOR item written as a node resumes the later pattern from it (gql:bind).
    let (b, p) = plan_with(
        "MATCH (a)-->(c) WITH collect(c) AS cs FOR n IN cs MATCH (n)-->(m) RETURN m",
        &NoFacts,
    );
    assert_eq!(
        stages(&b, &p),
        [
            "with [Aggregate] filters []",
            "for",
            "match: resume ^n; n-[_5]->m"
        ]
    );
    // The list reads a binding after an OPTIONAL MATCH part: through its label.
    let (b, p) = plan_with(
        "MATCH (a) OPTIONAL MATCH (a)-->(s) FOR x IN [s] RETURN x",
        &NoFacts,
    );
    assert!(reasons(&p, named(&b, "s")[0]).optional);
    // A leading LET produces no stage: the MATCH is the first segment.
    let (b, p) = plan_with("LET k = 1 MATCH (a {v: k}) RETURN a", &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a"]);
    assert!(p.stages.is_empty());
    // RETURN alone: no segment at all.
    let (_, p) = plan_with("RETURN 1 AS x", &NoFacts);
    assert!(p.ops.is_empty() && p.stages.is_empty());
}

// ----- stage 2a: the correlated lookup (design M3, E5) -----

/// The lookup of the first `Op::Lookup` among the first segment and the later stages.
fn first_lookup(p: &LogicalPlan) -> Option<&Lookup> {
    let stage_ops = p.stages.iter().flat_map(|stage| match stage {
        Stage::Match { ops, .. } | Stage::Optional { ops, .. } => ops.as_slice(),
        Stage::Materialize(_) | Stage::For(_) | Stage::Call(_) | Stage::Rows(_) => &[],
    });
    p.ops.iter().chain(stage_ops).find_map(|op| match op {
        Op::Lookup(l) => Some(l),
        _ => None,
    })
}

#[test]
fn a_lookup_starts_a_component_only_where_an_id_or_index_serves_it() {
    let indexed = Facts {
        indexes: vec![("person", "name"), ("person", "age")],
        ..Facts::default()
    };
    // By id: no index needed; the conjunct is the lookup's, not a filter.
    let q = "FOR id IN $ids MATCH (n) WHERE element_id(n) = id RETURN n";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(stages(&b, &p), ["for", "match: lookup n"]);
    let l = first_lookup(&p).unwrap();
    assert_eq!(l.by, LookupBy::Id);
    assert!(l.scan.filters.is_empty());
    // Either operand order.
    let (b, p) = plan_with(
        "FOR id IN $ids MATCH (n) WHERE id = element_id(n) RETURN n",
        &NoFacts,
    );
    assert_eq!(stages(&b, &p), ["for", "match: lookup n"]);
    // An inline property: only with an index on every scan label (E5).
    let q = "FOR x IN $xs MATCH (p:person {name: x}) RETURN p";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        ["for labelled", "match: scan p; filter [0]"]
    );
    let (b, p) = plan_with(q, &indexed);
    assert_eq!(stages(&b, &p), ["for", "match: lookup p"]);
    assert_eq!(
        first_lookup(&p).unwrap().by,
        LookupBy::Index("name".to_string())
    );
    let (_, p) = plan_with(
        "FOR x IN $xs MATCH (p:person|dog {name: x}) RETURN p",
        &indexed,
    );
    assert!(first_lookup(&p).is_none());
    // No scan label: no index can serve it.
    let (_, p) = plan_with("FOR x IN $xs MATCH (p {name: x}) RETURN p", &indexed);
    assert!(first_lookup(&p).is_none());
    // An id lookup wins over an index lookup.
    let (_, p) = plan_with(
        "FOR x IN $xs MATCH (p:person {name: x}) WHERE element_id(p) = x RETURN p",
        &indexed,
    );
    assert_eq!(first_lookup(&p).unwrap().by, LookupBy::Id);
    // The other side must not read the node itself.
    let (_, p) = plan_with(
        "FOR x IN $xs MATCH (p:person) WHERE p.name = p.nick RETURN p",
        &indexed,
    );
    assert!(first_lookup(&p).is_none());
    // Not an equality: no lookup.
    let (_, p) = plan_with(
        "FOR x IN $xs MATCH (p:person) WHERE p.age > x RETURN p",
        &indexed,
    );
    assert!(first_lookup(&p).is_none());
    // A second component of the first segment, against the first one.
    let q = "MATCH (a:person {name: 'marko'}), (b:person) WHERE b.age = a.age + 3 RETURN b";
    let (b, p) = plan_with(q, &indexed);
    assert_eq!(shape(&b, &p), ["scan a", "lookup b"]);
    assert_eq!(
        first_lookup(&p).unwrap().by,
        LookupBy::Index("age".to_string())
    );
    // The root scan never is one, also with an id equality to a constant.
    let (b, p) = plan_with("MATCH (n) WHERE element_id(n) = '1' RETURN n", &NoFacts);
    assert_eq!(shape(&b, &p), ["scan n"]);
    // A stage 1a query without an index or id equality plans as before.
    let (b, p) = plan_with(
        "MATCH (a:person {name: 'marko'}), (s:software) RETURN a, s",
        &NoFacts,
    );
    assert_eq!(shape(&b, &p), ["scan a", "scan s"]);
    // The value is read on the row: the FOR item gets no label for it (above: `for`, not
    // `for labelled`), unless a later read needs it.
    let (b, p) = plan_with(
        "FOR id IN $ids MATCH (n) WHERE element_id(n) = id RETURN n, id",
        &NoFacts,
    );
    assert_eq!(stages(&b, &p), ["for labelled", "match: lookup n"]);
}

// ----- subqueries (stage 2b) -----

/// One line per planned subquery body: `k@start: ops` (`start` the import the host stands on,
/// `-` for none).
fn subplans(b: &BoundQuery, p: &LogicalPlan) -> Vec<String> {
    p.subqueries
        .iter()
        .map(|s| {
            format!(
                "{}@{}: {}",
                s.subquery,
                s.current.map_or_else(|| "-".to_owned(), |c| name(b, c)),
                ops_shape(b, p, &s.ops).join("; ")
            )
        })
        .collect()
}

/// The element filters of the root scan.
fn root_filters(p: &LogicalPlan) -> &ElementFilters {
    match p.ops.first() {
        Some(Op::Scan(s)) => &s.filters,
        other => panic!("no scan first: {other:?}"),
    }
}

/// The bindings with the `subquery` reason, by name.
fn subquery_reads(b: &BoundQuery, p: &LogicalPlan) -> Vec<String> {
    (0..p.labelling.len())
        .filter(|&i| p.labelling[i].subquery)
        .map(|i| name(b, BindingId(i)))
        .collect()
}

#[test]
fn a_subquery_on_the_scanned_element_runs_last_on_it_without_a_label() {
    // "Who has no ...": the body starts at the current element (no `select`, no label).
    let q = "MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN count(*) AS c";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan p"]);
    assert_eq!(
        root_filters(&p),
        &ElementFilters {
            pushed: vec![],
            residual: vec![],
            exists: vec![0],
        }
    );
    assert_eq!(subplans(&b, &p), ["0@p: resume p; p-[_1]->_2"]);
    assert!(labelled(&b, &p).is_empty());
    // Design E11 on one element: pushed `has()`, residual `gql:filter`s (one holding a
    // subquery under `OR` after the plain ones), the `EXISTS` children last, each in text order.
    let q = "MATCH (p:person) WHERE EXISTS { (p)-->() } AND (p.x = 1 OR EXISTS { (p)<--() }) \
             AND p.name STARTS WITH 'm' AND p.age + 1 > 3 AND NOT (p)-[:knows]->() RETURN p";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        root_filters(&p),
        &ElementFilters {
            pushed: vec![2],
            residual: vec![3, 1],
            exists: vec![0, 4],
        }
    );
    assert_eq!(
        subplans(&b, &p),
        [
            "0@p: resume p; p-[_1]->_2",
            "1@p: resume p; p<-[_3]-_4",
            "2@p: resume p; p-[_5]->_6"
        ]
    );
}

#[test]
fn a_cross_subquery_conjunct_runs_after_the_other_filters_there() {
    // Two imports: the host filter runs where both are bound (on `b`); the body starts at the
    // current element and closes the pattern with a join on `a`.
    let q = "MATCH (a:person), (b:software) WHERE (a)-[:created]->(b) AND a.age > b.x RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "scan b", "filter [1, 0]"]);
    assert_eq!(subplans(&b, &p), ["0@b: resume b; b<-[_2]-a; join a"]);
    assert!(reasons(&p, named(&b, "a")[0]).join);
    // A subquery under `OR` (the expression hook) runs after the plain conjuncts, before the
    // fast-path ones.
    let q = "MATCH (a), (b) WHERE (a)-->(b) AND ((b)-->(a) OR a.x = 1) AND a.y = b.y RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "scan b", "filter [2, 1, 0]"]);
}

#[test]
fn an_import_read_away_from_the_current_element_in_the_body_is_labelled() {
    // `p` is read after the body moved on to `q`: the `subquery` reason. The binder no longer
    // marks it read for a body condition (only the body's own locals).
    let q = "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(q) WHERE q.age > p.age } RETURN 1 AS x";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(subplans(&b, &p), ["0@p: resume p; p-[_1]->q; filter [0]"]);
    assert_eq!(subquery_reads(&b, &p), ["p"]);
    let p_id = named(&b, "p")[0];
    assert!(!reasons(&p, p_id).read);
    assert!(b.binding(named(&b, "q")[0]).is_some_and(|q| q.read));
    // A body condition on the import alone runs at the body's start, on the current element:
    // no label at all.
    let q = "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(q) WHERE p.age > 30 } RETURN 1 AS x";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(subplans(&b, &p), ["0@p: filter [0]; resume p; p-[_1]->q"]);
    assert!(labelled(&b, &p).is_empty());
    // The same condition when the host stands on another element: read away at the start,
    // and the body moves to its import first.
    let q = "MATCH (a), (b) WHERE EXISTS { MATCH (a)-->(c) WHERE a.x = b.x } RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "scan b", "filter [1]"]);
    assert_eq!(subplans(&b, &p), ["0@b: filter [0]; resume ^a; a-[_2]->c"]);
    assert_eq!(subquery_reads(&b, &p), ["a"]);
    assert!(reasons(&p, named(&b, "a")[0]).resume);
}

#[test]
fn nested_subqueries_plan_inside_their_parents_body() {
    let q = "MATCH (a) WHERE EXISTS { MATCH (a)-->(b) WHERE EXISTS { MATCH (b)-->(c) \
             WHERE c.x = a.x } } RETURN 1 AS x";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a"]);
    assert_eq!(root_filters(&p).exists, [2]);
    // The inner host is a cross conjunct of the outer body (it imports `a` and `b`), placed
    // where `b` is bound; the inner body starts there.
    assert_eq!(
        subplans(&b, &p),
        [
            "0@a: resume a; a-[_1]->b; filter [1]",
            "1@b: resume b; b-[_3]->c; filter [0]"
        ]
    );
    assert_eq!(subquery_reads(&b, &p), ["a"]);
}

#[test]
fn a_body_is_planned_once_per_start() {
    // A LET value read on the element and in RETURN (after the walk moved on): two starts.
    let q = "MATCH (a) LET x = EXISTS { (a)-->() } MATCH (a)-->(b) WHERE x RETURN x AS y";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(shape(&b, &p), ["scan a", "a-[_3]->b"]);
    assert_eq!(root_filters(&p).exists, [0]);
    assert_eq!(
        subplans(&b, &p),
        ["0@-: resume ^a; a-[_1]->_2", "0@a: resume a; a-[_1]->_2"]
    );
    assert!(reasons(&p, named(&b, "a")[0]).resume);
    // The lowering finds a host's plan by the host's current element.
    let (a, b_id) = (named(&b, "a")[0], named(&b, "b")[0]);
    let found = |current| p.subquery_plan(&b, 0, current).map(|s| s.current);
    assert_eq!(found(Some(a)), Some(Some(a)));
    assert_eq!(found(Some(b_id)), Some(None));
    assert_eq!(found(None), Some(None));
    assert_eq!(p.subquery_plan(&b, 1, None), None);
    // Read twice at the same position: one plan.
    let q = "MATCH (a) LET x = EXISTS { (a)-->() } RETURN x AS y, x AS z";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(subplans(&b, &p), ["0@a: resume a; a-[_1]->_2"]);
}

#[test]
fn subqueries_in_columns_sort_keys_lists_and_rows() {
    // RETURN after an OPTIONAL MATCH: no current element, the import is read after the part.
    let q = "MATCH (a) OPTIONAL MATCH (a)-->(b) RETURN EXISTS { (b)-->() } AS x";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(subplans(&b, &p), ["0@-: resume ^b; b-[_3]->_4"]);
    let r = reasons(&p, named(&b, "b")[0]);
    assert!(r.resume && r.optional);
    // A materializing WITH's WHERE runs on its row: the body moves to the column (gql:bind)
    // and reads another column after leaving the row, so the row is labelled.
    let q = "MATCH (a)-->(b) WITH a, b LIMIT 5 WHERE EXISTS { MATCH (a)-->(c) \
             WHERE c.age > b.age } RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(subplans(&b, &p), ["0@-: resume ^a; a-[_5]->c; filter [0]"]);
    assert_eq!(
        stages(&b, &p),
        ["with [Project, Limit(Literal(5))] filters [1] labelled"]
    );
    // Without the read after leaving it, the row needs no label.
    let q = "MATCH (a)-->(b) WITH a, b LIMIT 5 WHERE EXISTS { (a)-->() } RETURN a";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        stages(&b, &p),
        ["with [Project, Limit(Literal(5))] filters [0]"]
    );
    // A column on the current element, a sort key (on the row map), a FOR list.
    let q = "MATCH (a)-->(b) RETURN b, EXISTS { (a)-->(b) } AS e ORDER BY EXISTS { (b)-->() }";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        subplans(&b, &p),
        [
            "0@b: resume b; b<-[_3]-a; join a",
            "1@-: resume ^b; b-[_4]->_5"
        ]
    );
    let q = "MATCH (a) FOR x IN [EXISTS { (a)-->() }] RETURN x";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(subplans(&b, &p), ["0@a: resume a; a-[_1]->_2"]);
    // A query without MATCH: an uncorrelated body is a child-leading scan (with its pushdown),
    // never a lookup, even where an index serves it.
    let facts = Facts {
        indexes: vec![("person", "name")],
        ..Facts::default()
    };
    let q = "RETURN EXISTS { MATCH (n:person {name: 'x'}) } AS x";
    let (b, p) = plan_with(q, &facts);
    assert!(p.ops.is_empty());
    assert_eq!(subplans(&b, &p), ["0@-: scan n"]);
    match &p.subqueries[0].ops[0] {
        Op::Scan(s) => assert_eq!(s.filters.pushed, [0]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn subquery_bodies_follow_the_segment_rules() {
    let facts = Facts {
        indexes: vec![("person", "name")],
        ..Facts::default()
    };
    // A later unanchored component of a correlated body: a lookup where an index serves it.
    let q = "MATCH (a:person) WHERE EXISTS { MATCH (a)-->(b), (c:person {name: a.name}) } \
             RETURN 1 AS x";
    let (b, p) = plan_with(q, &facts);
    assert_eq!(subplans(&b, &p), ["0@a: resume a; a-[_1]->b; lookup c"]);
    assert_eq!(subquery_reads(&b, &p), ["a"]);
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        subplans(&b, &p),
        ["0@a: resume a; a-[_1]->b; scan c; filter [0]"]
    );
    // Trail per body clause only (design E8): `g` checks `f`, never the outer `e`.
    let q = "MATCH (a)-[e]->(b) WHERE EXISTS { MATCH (a)-[f]->(c)-[g]->(d) } RETURN 1 AS x";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        subplans(&b, &p),
        ["0@a: resume a; a-[f]->c; c-[g]->d trail!=f"]
    );
    // An outer edge bound again inside: an edge join.
    let q = "MATCH (a)-[e]->(b) WHERE EXISTS { MATCH (b)<-[e]-(a) } RETURN 1 AS x";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        subplans(&b, &p),
        ["0@b: resume b; b<-[e]-a edge_join; join a"]
    );
    assert!(reasons(&p, named(&b, "e")[0]).join);
    // A host on an edge (its inline WHERE): the body reaches the edge with an edge join.
    let q = "MATCH (a)-[e WHERE EXISTS { MATCH (x)-[e]->(:software) }]->(b) RETURN 1 AS x";
    let (b, p) = plan_with(q, &NoFacts);
    match &p.ops[1] {
        Op::Expand(e) => assert_eq!(e.edge_filters.exists, [0]),
        other => panic!("{other:?}"),
    }
    assert_eq!(subplans(&b, &p), ["0@-: scan _4; _4<-[e]-x edge_join"]);
}

#[test]
fn subquery_plans_are_deterministic_and_bind_every_local() {
    let heads = [
        "MATCH (a)-[e]->(b)",
        "OPTIONAL MATCH (a)",
        "MATCH (a)-[e]->{1,2}(b)",
        "MATCH p = (a)-->(b)",
        "FOR a IN [1]",
        "MATCH (a) WITH a, count(*) AS c",
        "MATCH (a:person {name: 'x'}), (b:person)",
    ];
    let preds = [
        "EXISTS { MATCH (a)-->(c) }",
        "EXISTS { (a)-[e]->(b) WHERE b.x = a.x }",
        "NOT (a)-->(b)",
        "EXISTS ((c)-->())",
        "EXISTS { MATCH (a)-->(b) MATCH (b)-->(c) WHERE EXISTS { (c)-->(a) } }",
        "EXISTS { MATCH q = (a)-->() WHERE path_length(q) > 1 }",
        "EXISTS { MATCH (a)-->(c), (d:person {name: a.name}) WHERE d.x = c.x }",
        "(a)-->() OR a.x = 1",
    ];
    let tails = [
        "RETURN a",
        "RETURN a, (a)-->() AS h ORDER BY h",
        "RETURN count(*) + CASE WHEN (a)-->() THEN 1 END AS k",
        "MATCH (c) WHERE EXISTS { (c)-->(a) } RETURN c",
    ];
    let facts = Facts {
        indexes: vec![("person", "name")],
        ..Facts::default()
    };
    let mut planned = 0;
    for h in heads {
        for pr in preds {
            for t in tails {
                for q in [
                    format!("{h} WHERE {pr} {t}"),
                    format!("{h} FILTER {pr} {t}"),
                ] {
                    let Ok(parsed) = parse(&q) else { continue };
                    let Ok(b) = bind(&parsed) else { continue };
                    for f in [&NoFacts as &dyn PlannerFacts, &facts] {
                        let p = plan(&b, f, None);
                        assert_eq!(p, plan(&b, f, None), "{q}");
                        for s in &p.subqueries {
                            let sub = &b.subqueries[s.subquery];
                            let reached: Vec<BindingId> = s
                                .ops
                                .iter()
                                .flat_map(|op| match op {
                                    Op::Scan(x) | Op::Anchor(x) => vec![x.binding],
                                    Op::Lookup(l) => vec![l.scan.binding],
                                    Op::Resume(r) => vec![r.binding],
                                    Op::Expand(e) => vec![e.edge, e.to],
                                    Op::Join { .. } | Op::Filter(_) => vec![],
                                })
                                .collect();
                            for local in b.bindings.iter().filter(|x| {
                                x.segment == sub.segment
                                    && matches!(x.kind, BindingKind::Node | BindingKind::Edge)
                            }) {
                                assert!(reached.contains(&local.id), "{q}: {:?}", local.name);
                            }
                            // Every body conjunct is placed exactly once.
                            let mut placed: Vec<usize> = s
                                .ops
                                .iter()
                                .flat_map(|op| match op {
                                    Op::Filter(ids) => ids.clone(),
                                    Op::Scan(x) | Op::Anchor(x) => element_ids(&x.filters),
                                    Op::Lookup(l) => {
                                        let mut ids = element_ids(&l.scan.filters);
                                        ids.push(l.conjunct);
                                        ids
                                    }
                                    Op::Expand(e) => {
                                        let mut ids = element_ids(&e.edge_filters);
                                        ids.extend(element_ids(&e.to_filters));
                                        ids
                                    }
                                    Op::Resume(_) | Op::Join { .. } => vec![],
                                })
                                .collect();
                            placed.sort_unstable();
                            let own: Vec<usize> = (0..b.conjuncts.len())
                                .filter(|&c| b.conjuncts[c].segment == sub.segment)
                                .collect();
                            assert_eq!(placed, own, "{q}");
                        }
                        planned += 1;
                    }
                }
            }
        }
    }
    assert!(planned > 100, "{planned}");
}

fn element_ids(f: &ElementFilters) -> Vec<usize> {
    f.pushed
        .iter()
        .chain(&f.residual)
        .chain(&f.exists)
        .copied()
        .collect()
}

// ----- stage 2d: composite queries, CALL, whole-query subquery bodies -----

/// One pipeline: its match pipeline (as [`shape`]), then one line per stage (as [`stages`]).
fn pipeline_lines(b: &BoundQuery, p: &LogicalPlan, pipeline: &PipelinePlan) -> Vec<String> {
    let mut only = p.clone();
    only.body.first.first = pipeline.clone();
    let mut lines = shape(b, &only);
    lines.extend(stages(b, &only));
    lines
}

/// Every pipeline of `plan` (its `CALL` bodies after it, indented), one line each: `|` between
/// operands of a block, `NEXT` between blocks, the result pipeline at the end.
fn tree(b: &BoundQuery, p: &LogicalPlan, plan: &CompositePlan) -> Vec<String> {
    let mut out = Vec::new();
    for (k, block) in plan.blocks().enumerate() {
        for (i, pipeline) in block.operands().enumerate() {
            let prefix = match (k, i) {
                (0, 0) => String::new(),
                (_, 0) => "NEXT ".to_owned(),
                _ if block.distinct => "UNION ".to_owned(),
                _ => "UNION ALL ".to_owned(),
            };
            out.push(format!(
                "{prefix}{} => {:?}",
                pipeline_lines(b, p, pipeline).join("; "),
                pipeline.result
            ));
            for stage in &pipeline.stages {
                if let Stage::Call(call) = stage {
                    out.extend(tree(b, p, &call.plan).into_iter().map(|l| format!("  {l}")));
                }
            }
        }
    }
    out
}

/// The `CALL` stages of the first pipeline.
fn calls(p: &LogicalPlan) -> Vec<&CallStage> {
    p.stages
        .iter()
        .filter_map(|stage| match stage {
            Stage::Call(call) => Some(call),
            _ => None,
        })
        .collect()
}

/// `tree` of the whole query.
fn top(q: &str, facts: &dyn PlannerFacts) -> Vec<String> {
    let (b, p) = plan_with(q, facts);
    tree(&b, &p, &p.body)
}

/// Facts with a property index on `name` of `person` and `software`.
fn name_indexes() -> Facts {
    Facts {
        indexes: vec![("person", "name"), ("software", "name")],
        ..Facts::default()
    }
}

#[test]
fn a_linear_query_plans_one_block_of_one_pipeline() {
    let (_, p) = plan_with(
        "MATCH (a)-->(b) WITH a, count(b) AS n RETURN a, n",
        &NoFacts,
    );
    assert!(p.body.next.is_empty());
    assert!(p.body.first.unions.is_empty() && !p.body.first.distinct);
    // The plan dereferences to its only pipeline.
    assert_eq!(p.ops, p.body.first.first.ops);
    assert_eq!(p.stages.len(), 1);
    assert_eq!(p.result, [ResultOp::Project]);
}

#[test]
fn union_operands_are_pipelines_of_their_own() {
    // Each operand is planned like a top-level query: its own root scan, with its pushdown and
    // index (never a lookup), its own result pipeline.
    let q = "MATCH (a:person {name: 'marko'}) RETURN a.name AS n \
             UNION MATCH (b:software {name: 'lop'}) RETURN b.name AS n";
    let (b, p) = plan_with(q, &name_indexes());
    assert_eq!(
        tree(&b, &p, &p.body),
        ["scan a => [Project]", "UNION scan b => [Project]"]
    );
    assert!(p.body.first.distinct);
    for operand in p.body.first.operands() {
        let Some(Op::Scan(scan)) = operand.ops.first() else {
            panic!("a root scan: {:?}", operand.ops)
        };
        assert_eq!(scan.filters.pushed.len(), 1);
    }
    // ORDER BY / OFFSET / LIMIT belong to their operand (E15).
    assert_eq!(
        top(
            "MATCH (a:person) RETURN a.name AS n ORDER BY n LIMIT 2 \
             UNION ALL MATCH (b:software) RETURN b.name AS n",
            &NoFacts,
        ),
        [
            "scan a => [Project, Order, Limit(Literal(2))]",
            "UNION ALL scan b => [Project]"
        ]
    );
    let (_, p) = plan_with("RETURN 1 AS x UNION ALL RETURN 2 AS x", &NoFacts);
    assert!(!p.body.first.distinct);
    // Operands without MATCH start from the unit row.
    assert!(p.body.first.operands().all(|o| o.ops.is_empty()));
}

#[test]
fn next_blocks_start_from_the_rows_before() {
    // After a UNION block: a Rows stage, its columns read like a WITH row's (`#n<k>` only
    // when read after a later stage left the row).
    let union = "MATCH (a:person) RETURN a AS x UNION ALL MATCH (b:software) RETURN b AS x";
    assert_eq!(
        top(
            &format!("{union} NEXT MATCH (x)-->(y) RETURN x, y"),
            &NoFacts
        ),
        [
            "scan a => [Project]",
            "UNION ALL scan b => [Project]",
            "NEXT rows labelled; match: resume ^x; x-[_3]->y => [Project]"
        ]
    );
    assert_eq!(
        top(&format!("{union} NEXT RETURN x ORDER BY x.name"), &NoFacts),
        [
            "scan a => [Project]",
            "UNION ALL scan b => [Project]",
            "NEXT rows => [Project, Order]"
        ]
    );
    // A UNION block after NEXT: every operand starts from the same rows.
    assert_eq!(
        top(
            "MATCH (a:person) RETURN a NEXT MATCH (a)-[:created]->(b) RETURN b \
             UNION MATCH (a)-[:knows]->(b) RETURN b",
            &NoFacts,
        ),
        [
            "scan a => [Project]",
            "NEXT rows; match: resume ^a; a-[_2]->b => [Project]",
            "UNION rows; match: resume ^a; a-[_5]->b => [Project]"
        ]
    );
    // NEXT after a linear query is the WITH it desugars to (E19): one pipeline, the same plan.
    let (b, p) = plan_with(
        "MATCH (a:person) RETURN a NEXT MATCH (a)-[:created]->(b) RETURN b",
        &NoFacts,
    );
    let (tb, tp) = plan_with(
        "MATCH (a:person) WITH a MATCH (a)-[:created]->(b) RETURN b",
        &NoFacts,
    );
    assert!(p.body.next.is_empty());
    assert_eq!(tree(&b, &p, &p.body), tree(&tb, &tp, &tp.body));
}

#[test]
fn call_is_a_stage_whose_body_starts_on_the_incoming_row() {
    let q = "MATCH (a:person) CALL (a) { MATCH (a)-[:created]->(s) RETURN s.name AS sn } \
             RETURN a.name AS name, sn";
    let (b, p) = plan_with(q, &NoFacts);
    // The body continues from the current element without moving.
    assert_eq!(
        tree(&b, &p, &p.body),
        [
            "scan a; call => [Project]",
            "  resume a; a-[_1]->s => [Project]"
        ]
    );
    let call = calls(&p)[0];
    assert_eq!(call.imports, named(&b, "a"));
    assert!(!call.optional && !call.labelled);
    // A returned node continues a later pattern from the CALL row (read there: no row label).
    assert_eq!(
        top(
            "MATCH (a:person) CALL (a) { MATCH (a)-[:created]->(s) RETURN s } \
             MATCH (s)<--(c) RETURN a, c",
            &NoFacts,
        ),
        [
            "scan a; call; match: resume ^s; s<-[_4]-c => [Project]",
            "  resume a; a-[_1]->s => [Project]"
        ]
    );
    // A body starting away from the current element moves to its import (a label).
    let q = "MATCH (a)-->(b) CALL (a) { MATCH (a)-->(c) RETURN c } RETURN c";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        tree(&b, &p, &p.body),
        [
            "scan a; a-[_1]->b; call => [Project]",
            "  resume ^a; a-[_3]->c => [Project]"
        ]
    );
    assert!(reasons(&p, named(&b, "a")[0]).resume);
    // After a CALL there is no current element: a later pattern resumes through a label, and
    // a returned column read after it left the row labels the row (`#c<k>`).
    let q = "MATCH (a) CALL (a) { RETURN 1 AS one } MATCH (a)-->(b) RETURN b, one";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        tree(&b, &p, &p.body),
        [
            "scan a; call labelled; match: resume ^a; a-[_2]->b => [Project]",
            "   => [Project]"
        ]
    );
    assert!(reasons(&p, named(&b, "a")[0]).resume);
}

#[test]
fn an_optional_call_reads_its_columns_through_the_row_label() {
    let body = "(a) { MATCH (a)-[:created]->(s) RETURN s.name AS sn } RETURN sn";
    let (_, p) = plan_with(&format!("MATCH (a:person) OPTIONAL CALL {body}"), &NoFacts);
    let call = calls(&p)[0];
    assert!(call.optional && call.labelled);
    // Without OPTIONAL the column is read on the CALL's row.
    let (_, p) = plan_with(&format!("MATCH (a:person) CALL {body}"), &NoFacts);
    assert!(!calls(&p)[0].labelled);
}

#[test]
fn call_bodies_follow_the_body_rules() {
    // Conditions over imports only run first; a component without an import is a
    // child-leading scan.
    assert_eq!(
        top(
            "MATCH (a) CALL (a) { MATCH (s:software) WHERE a.age > 30 AND s.lang = a.lang \
             RETURN s } RETURN s",
            &NoFacts,
        ),
        [
            "scan a; call => [Project]",
            "  filter [0]; scan s; filter [1] => [Project]"
        ]
    );
    // The body runs per row: an equality reading the row is a correlated lookup where an
    // index serves it; a constant one stays a scan with its pushdown.
    let correlated = "MATCH (a:person) CALL (a) { MATCH (s:software {name: a.name}) RETURN s } \
                      RETURN s";
    assert_eq!(
        top(correlated, &name_indexes()),
        ["scan a; call => [Project]", "  lookup s => [Project]"]
    );
    assert_eq!(
        top(correlated, &NoFacts),
        [
            "scan a; call => [Project]",
            "  scan s; filter [0] => [Project]"
        ]
    );
    assert_eq!(
        top(
            "MATCH (a:person) CALL (a) { MATCH (s:software {name: 'lop'}) RETURN s } RETURN s",
            &name_indexes(),
        ),
        ["scan a; call => [Project]", "  scan s => [Project]"]
    );
    // Every operand of a UNION body starts on the incoming row; NEXT inside from its rows.
    assert_eq!(
        top(
            "MATCH (a) CALL (a) { MATCH (a)-->(b) RETURN b UNION MATCH (a)<--(b) RETURN b \
             NEXT RETURN b ORDER BY b.name LIMIT 1 } RETURN b",
            &NoFacts,
        ),
        [
            "scan a; call => [Project]",
            "  resume a; a-[_1]->b => [Project]",
            "  UNION resume a; a<-[_3]-b => [Project]",
            "  NEXT rows => [Project, Order, Limit(Literal(1))]"
        ]
    );
    // Nested bodies.
    assert_eq!(
        top(
            "MATCH (a) CALL (a) { CALL (a) { MATCH (a)-->(b) RETURN b } RETURN b AS c } RETURN c",
            &NoFacts,
        ),
        [
            "scan a; call => [Project]",
            "  call => [Project]",
            "    resume a; a-[_1]->b => [Project]"
        ]
    );
    // A body read after an OPTIONAL MATCH part reads its import through a label.
    let q = "MATCH (a) OPTIONAL MATCH (a)-->(o) CALL (a) { MATCH (a)-->(c) RETURN c } RETURN o, c";
    let (b, p) = plan_with(q, &NoFacts);
    assert_eq!(
        tree(&b, &p, &p.body),
        [
            "scan a; optional: resume a; a-[_1]->o witness o; call => [Project]",
            "  resume ^a; a-[_3]->c => [Project]"
        ]
    );
    assert!(reasons(&p, named(&b, "a")[0]).optional);
}

#[test]
fn a_row_before_a_call_is_labelled_only_when_read_after_it() {
    // The body reads the WITH row it runs on: no row label.
    let (b, p) = plan_with(
        "MATCH (a) WITH a, count(*) AS n CALL (n) { RETURN n + 1 AS m } RETURN m",
        &NoFacts,
    );
    assert_eq!(
        tree(&b, &p, &p.body),
        [
            "scan a; with [Aggregate] filters []; call => [Project]",
            "   => [Project]"
        ]
    );
    // A column of it read after the CALL: labelled (`#w<k>`).
    let (b, p) = plan_with(
        "MATCH (a) WITH a, count(*) AS n CALL (a) { MATCH (a)-->(c) RETURN c } RETURN n, c",
        &NoFacts,
    );
    assert_eq!(
        tree(&b, &p, &p.body),
        [
            "scan a; with [Aggregate] filters [] labelled; call => [Project]",
            "  resume ^a; a-[_3]->c => [Project]"
        ]
    );
}

#[test]
fn subqueries_inside_call_bodies_are_hosted_there() {
    let (b, p) = plan_with(
        "MATCH (a:person) CALL (a) { MATCH (a)-->(x) WHERE EXISTS { MATCH (x)-->(y) } \
         RETURN x } RETURN x",
        &NoFacts,
    );
    assert_eq!(subplans(&b, &p), ["0@x: resume x; x-[_3]->y"]);
}

/// One line per planned whole-query body: `k@start`, then its `tree`, indented.
fn query_subplans(b: &BoundQuery, p: &LogicalPlan) -> Vec<String> {
    let mut out = Vec::new();
    for s in &p.subqueries {
        let Some(query) = &s.query else { continue };
        assert!(s.ops.is_empty());
        out.push(format!(
            "{}@{}",
            s.subquery,
            s.current.map_or_else(|| "-".to_owned(), |c| name(b, c))
        ));
        out.extend(tree(b, p, query).into_iter().map(|l| format!("  {l}")));
    }
    out
}

#[test]
fn whole_query_subquery_bodies_plan_as_composites() {
    let (b, p) = plan_with(
        "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) WITH s, count(*) AS c \
         WHERE c > 0 RETURN s } RETURN p.name",
        &NoFacts,
    );
    assert_eq!(
        query_subplans(&b, &p),
        [
            "0@p",
            "  resume p; p-[_1]->s; with [Aggregate] filters [0] => [Project]"
        ]
    );
    // A UNION body: each operand starts on the host's element.
    let (b, p) = plan_with(
        "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(x) RETURN x UNION MATCH (p)<--(x) \
         RETURN x } RETURN p",
        &NoFacts,
    );
    assert_eq!(
        query_subplans(&b, &p),
        [
            "0@p",
            "  resume p; p-[_1]->x => [Project]",
            "  UNION resume p; p<-[_3]-x => [Project]"
        ]
    );
    // An import read in a column after the body moved on gets the `subquery` reason.
    let (b, p) = plan_with(
        "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) RETURN p.name AS n } \
         RETURN p.name",
        &NoFacts,
    );
    assert_eq!(subquery_reads(&b, &p), ["p"]);
    let (b, p) = plan_with(
        "MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) RETURN s.name AS n } \
         RETURN p.name",
        &NoFacts,
    );
    assert!(subquery_reads(&b, &p).is_empty());
    // A COUNT { } body in a column, and a correlated lookup at the start of a body.
    let (b, p) = plan_with(
        "MATCH (p:person) RETURN COUNT { MATCH (s:software {name: p.name}) RETURN s } AS c",
        &name_indexes(),
    );
    assert_eq!(query_subplans(&b, &p), ["0@p", "  lookup s => [Project]"]);
}

/// Every node binding a pipeline or body of `p` starts at or reaches, by id.
fn reached(p: &LogicalPlan) -> BTreeSet<BindingId> {
    fn ops_reached(ops: &[Op], out: &mut BTreeSet<BindingId>) {
        for op in ops {
            match op {
                Op::Scan(s) | Op::Anchor(s) => {
                    out.insert(s.binding);
                }
                Op::Lookup(l) => {
                    out.insert(l.scan.binding);
                }
                Op::Resume(r) => {
                    out.insert(r.binding);
                }
                Op::Expand(e) => {
                    out.insert(e.from);
                    out.insert(e.to);
                }
                Op::Join { .. } | Op::Filter(_) => {}
            }
        }
    }
    fn composite_reached(plan: &CompositePlan, out: &mut BTreeSet<BindingId>) {
        for pipeline in plan.blocks().flat_map(|block| block.operands()) {
            ops_reached(&pipeline.ops, out);
            for stage in &pipeline.stages {
                match stage {
                    Stage::Match { ops, .. } | Stage::Optional { ops, .. } => {
                        ops_reached(ops, out);
                    }
                    Stage::Call(call) => composite_reached(&call.plan, out),
                    Stage::Materialize(_) | Stage::For(_) | Stage::Rows(_) => {}
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    composite_reached(&p.body, &mut out);
    for s in &p.subqueries {
        ops_reached(&s.ops, &mut out);
        if let Some(query) = &s.query {
            composite_reached(query, &mut out);
        }
    }
    out
}

#[test]
fn composite_plans_are_deterministic_and_reach_every_pattern_node() {
    let queries = [
        "MATCH (a:person)-->(b) RETURN b AS x UNION MATCH (c:software)<--(d) RETURN d AS x \
         NEXT MATCH (x)-->(y) RETURN y",
        "MATCH (a) CALL (a) { MATCH (a)-->(b), (c:software {name: a.name}) RETURN c \
         UNION ALL OPTIONAL MATCH (a)<--(c) RETURN c } MATCH (c)-->(d) RETURN d",
        "MATCH (a) OPTIONAL CALL (a) { CALL (a) { MATCH (a)-[e]->{1,2}(b) RETURN b } \
         MATCH (b)-->(c) WHERE EXISTS { MATCH (c)-->(z) RETURN z } RETURN c } RETURN c",
        "MATCH (p:person) WHERE EXISTS { MATCH (p)-->(x) WITH x, count(*) AS n \
         MATCH (x)<--(y) RETURN y UNION MATCH (p)<--(y) RETURN y } RETURN p",
        "FOR i IN [1, 2] CALL (i) { MATCH (s) WHERE s.age = i RETURN s } RETURN s",
    ];
    for q in queries {
        for facts in [&NoFacts as &dyn PlannerFacts, &name_indexes()] {
            let (b, p) = plan_with(q, facts);
            assert_eq!(p, plan(&b, facts, None), "{q}");
            let reached = reached(&p);
            for m in &b.matches {
                for path in &m.paths {
                    for node in std::iter::once(path.start).chain(path.steps.iter().map(|s| s.node))
                    {
                        assert!(reached.contains(&node), "{q}: {} unreached", name(&b, node));
                    }
                }
            }
        }
    }
}

/// The deepest accepted bodies (`bind::MAX_BODY_DEPTH`) with the tallest expression inside
/// bind and plan on a test thread's 2 MiB stack (debug build; the planner recurses once per
/// `CALL` body, see the 2d.T3 report for the numbers).
#[test]
fn the_deepest_bodies_plan_within_the_stack() {
    use crate::gql::bind::MAX_BODY_DEPTH;
    let ors = |n: usize| {
        (0..n)
            .map(|i| format!("x = {i}"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let mut calls = format!("WITH 1 AS x RETURN {} AS y", ors(500));
    for _ in 0..MAX_BODY_DEPTH {
        calls = format!("CALL {{ {calls} UNION ALL RETURN 2 AS y }} RETURN y");
    }
    let mut bodies = format!("EXISTS {{ LET x = 1 FILTER {} RETURN x }}", ors(440));
    for _ in 1..MAX_BODY_DEPTH {
        bodies = format!("EXISTS {{ MATCH (n) WITH n WHERE {bodies} RETURN n }}");
    }
    let mut matches = String::from("MATCH (n)-->(m) RETURN m");
    for _ in 0..MAX_BODY_DEPTH {
        matches = format!("MATCH (n) CALL (n) {{ {matches} }} RETURN m");
    }
    for q in [calls, format!("MATCH (n) WHERE {bodies} RETURN n"), matches] {
        let (b, p) = plan_with(&q, &NoFacts);
        assert_eq!(p, plan(&b, &NoFacts, None));
    }
}
