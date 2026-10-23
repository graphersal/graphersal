//! The lowering of `MERGE` (stage 3e.T3): the match child, the create child, the checks, the
//! `ON` items and the row label, read as text from the lowered instructions.

use super::*;

/// The lowering of `query` on the modern graph.
fn lowered(query: &str) -> (String, Vec<MergeText>) {
    lowered_on(&crate::TraversalGraph::tinkerpop_modern(), query)
}

fn lowered_on(graph: &crate::TraversalGraph, query: &str) -> (String, Vec<MergeText>) {
    merge_lowering(query, graph).unwrap_or_else(|e| panic!("{query}: {e}"))
}

/// The match and create children of the one `MERGE` of `query`.
fn children(query: &str) -> (String, String) {
    let (_, merges) = lowered(query);
    assert_eq!(merges.len(), 1, "{query}");
    let m = &merges[0];
    (m.matcher.clone(), m.creator.clone())
}

#[test]
fn merge_lowers_a_node_to_a_scan_and_an_insert_with_one_record() {
    let (outer, merges) = lowered("MERGE (p:person {name: 'marko'})");
    // The merge step (the marker here) ends the query (a write ending: `discard()`).
    assert!(outer.ends_with(".discard()"), "{outer}");
    let m = &merges[0];
    assert_eq!(
        m.matcher,
        r#"__.v().has_label("person").has("name", P.eq("marko")).gql:project("p")"#
    );
    assert_eq!(
        m.creator,
        r#"__.gql:insert_node(["person"], "{name: 'marko'}").gql:project("p")"#
    );
    assert_eq!(m.checks, ["name"]);
    assert_eq!(m.items, (0, 0));
    assert_eq!(
        m.create_labels,
        [(ElementKind::Vertex, Some(CompactString::from("person")))]
    );
    // A node without labels: one label-less create request.
    let (_, merges) = lowered("MERGE (n) RETURN n");
    assert_eq!(merges[0].matcher, r#"__.v().gql:project("n")"#);
    assert_eq!(merges[0].create_labels, [(ElementKind::Vertex, None)]);
    // Labels as a conjunction, every value checked (nodes, then edges, in create order).
    let (_, merges) = lowered("MERGE (n:a:b {k: 1, j: 2})-[:t {w: 3}]->(m:c) RETURN n");
    let m = &merges[0];
    assert_eq!(
        m.matcher,
        r##"__.v().has_label("a").has_label("b").has("k", P.eq(1)).has("j", P.eq(2)).as("#3").out_e("t").has("w", P.eq(3)).as("#m1").in_v().has_label("c").gql:project("n", "#m1", "m")"##
    );
    assert_eq!(
        m.creator,
        r##"__.gql:insert_node(["a", "b"], "{k: 1, j: 2}").as("n").gql:insert_node(["c"]).as("m").gql:insert_edge("t", "n", "m", "{w: 3}").gql:project("n", "#m1", "m")"##
    );
    assert_eq!(m.checks, ["k", "j", "w"]);
    let label = |kind, l: &str| (kind, Some(CompactString::from(l)));
    assert_eq!(
        m.create_labels,
        [
            label(ElementKind::Vertex, "a"),
            label(ElementKind::Vertex, "b"),
            label(ElementKind::Vertex, "c"),
            label(ElementKind::Edge, "t"),
        ]
    );
}

#[test]
fn merge_between_bound_ends_resumes_and_joins() {
    let q = "MATCH (a:person), (s:software) MERGE (a)-[e:made]->(s) \
             ON MATCH SET e.n = s.name, a.seen = true RETURN e, a.name";
    let (outer, merges) = lowered(q);
    // Both ends keep their labels (the join, the ON items, the create child read them).
    assert!(
        outer.contains(r#".as("s")"#) && outer.contains(r#".as("a")"#),
        "{outer}"
    );
    let m = &merges[0];
    assert_eq!(
        m.matcher,
        r##"__.out_e("made").as("#3").in_v().where(P.eq("s")).gql:project("e")"##
    );
    assert_eq!(
        m.creator,
        r#"__.gql:insert_edge("made", "a", "s").gql:project("e")"#
    );
    assert_eq!(m.items, (0, 2));
    // A new node between two bound ends.
    let (matcher, creator) = children(
        "MATCH (a), (b) WHERE a.name = 'marko' AND b.name = 'josh' \
         MERGE (a)-[:x]->(m:mid)<-[:y]-(b) RETURN m",
    );
    assert_eq!(
        matcher,
        r##"__.out_e("y").as("#m3").in_v().has_label("mid").as("#6").in_e("x").as("#m1").out_v().where(P.eq("a")).gql:project("#m1", "m", "#m3")"##
    );
    assert_eq!(
        creator,
        r##"__.gql:insert_node(["mid"]).as("m").gql:insert_edge("x", "a", "m").as("#2").gql:insert_edge("y", "b", "m").gql:project("#m1", "m", "#m3")"##
    );
}

#[test]
fn merge_undirected_matches_either_direction_and_creates_left_to_right() {
    let (matcher, creator) = children(
        "MATCH (a:person {name: 'marko'}), (b:software) MERGE (a)-[r:created]-(b) RETURN r",
    );
    assert_eq!(
        matcher,
        r##"__.union(__.out_e("created").as("#3").in_v(), __.in_e("created").gql:not_self_loop().as("#3").out_v()).where(P.eq("a")).gql:project("r")"##
    );
    // From the node before the edge to the node after it (W33).
    assert_eq!(
        creator,
        r#"__.gql:insert_edge("created", "a", "b").gql:project("r")"#
    );
}

#[test]
fn merge_path_variable_is_assembled_alike_in_both_children() {
    let (matcher, creator) = children("MERGE path = (:x)-[:t]->(:y) RETURN path");
    assert_eq!(
        matcher,
        r##"__.v().has_label("x").as("#m0").out_e("t").as("#m1").in_v().has_label("y").as("#m2").gql:project("#m0", "#m1", "#m2", "path")"##
    );
    // Every created element labelled: the path is read from their labels.
    assert_eq!(
        creator,
        r##"__.gql:insert_node(["x"]).as("#0").gql:insert_node(["y"]).as("#2").gql:insert_edge("t", "(#0)", "(#2)").as("#1").gql:project("#m0", "#m1", "#m2", "path")"##
    );
    // A bound end that is a `WITH` column has no label: both children move to it first.
    let (matcher, creator) =
        children("MATCH (a:person) WITH a, count(*) AS c MERGE p = (a)-[:k]->(b:z) RETURN p, c");
    assert!(
        matcher.starts_with(r##"__.gql:bind("a").as("#1")"##),
        "{matcher}"
    );
    assert_eq!(
        creator,
        r##"__.gql:bind("a").as("#1").gql:insert_node(["z"]).as("b").gql:insert_edge("k", "a", "b").as("#3").gql:project("#m1", "b", "p")"##
    );
}

#[test]
fn merge_in_a_call_body_and_after_another_merge() {
    let (outer, merges) =
        lowered("MATCH (p:person) CALL (p) { MERGE (p)-[:likes]->(:fruit) } RETURN count(*)");
    assert!(outer.contains(".optional("), "{outer}");
    assert_eq!(
        merges[0].matcher,
        r##"__.out_e("likes").as("#m1").in_v().has_label("fruit").gql:project("#m1", "#m2")"##
    );
    assert_eq!(
        merges[0].creator,
        r##"__.gql:insert_node(["fruit"]).as("#2").gql:insert_edge("likes", "p", "(#2)").gql:project("#m1", "#m2")"##
    );
    // The second MERGE reads the first one's record column: it moves to it.
    let (_, merges) = lowered("MERGE (a:x) MERGE (a)-[:t]->(b:y) RETURN b");
    assert_eq!(merges.len(), 2);
    assert_eq!(
        merges[1].matcher,
        r##"__.gql:bind("a").out_e("t").as("#m1").in_v().has_label("y").gql:project("#m1", "b")"##
    );
    assert_eq!(
        merges[1].creator,
        r##"__.gql:insert_node(["y"]).as("b").gql:insert_edge("t", "a", "b").gql:project("#m1", "b")"##
    );
}

#[test]
fn merge_record_row_label_only_when_read_after_leaving_it() {
    let (outer, _) = lowered("MERGE (n:x) ON MATCH SET n.k = 1 MATCH (m:y) RETURN n, m");
    assert!(
        outer.contains(r##".as("#m1").v().has_label("y")"##),
        "{outer}"
    );
    let (outer, merges) = lowered("MERGE (n:x) ON MATCH SET n.k = 1 RETURN n");
    assert!(!outer.contains("#m"), "{outer}");
    assert_eq!(merges[0].items, (0, 1));
}

#[test]
fn merge_on_a_value_the_row_gives_uses_the_index_lookup() {
    let mut graph = crate::TraversalGraph::tinkerpop_modern();
    graph
        .traversal_mut()
        .create_index(
            "person_name",
            crate::catalog::PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let (_, merges) = lowered_on(
        &graph,
        "FOR x IN ['a', 'b'] MERGE (n:person {name: x}) RETURN n",
    );
    assert_eq!(
        merges[0].matcher,
        r#"__.gql:lookup("(n:person)", "name: x") [index: vertex person.name].has_label("person").gql:project("n")"#
    );
    // Without the index: the same lookup, scanning the label and comparing directly.
    let (_, merges) = lowered("FOR x IN ['a', 'b'] MERGE (n:person {name: x}) RETURN n");
    assert_eq!(
        merges[0].matcher,
        r#"__.gql:lookup("(n:person)", "name: x") [scan: vertex person.name].has_label("person").gql:project("n")"#
    );
}

#[test]
fn merge_on_items_with_a_subquery_host_it_on_the_record() {
    let (_, merges) = lowered(
        "MATCH (a:person) MERGE (a)-[:k]->(b:z) ON CREATE SET b.n = COUNT { MATCH (a)-->(x) } \
         RETURN b",
    );
    assert_eq!(merges[0].items, (1, 0));
}
