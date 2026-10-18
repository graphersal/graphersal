use super::*;
use crate::gql::ast::Literal;
use crate::gql::bind::bind;
use crate::gql::parser::parse;
use crate::gql::plan::{PushOp, PushOperand, classify};

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
