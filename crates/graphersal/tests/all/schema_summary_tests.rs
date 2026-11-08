//! The schema summary (`GraphTraversalSource::schema_summary`): the text map of labels, keys,
//! types, edge endpoint pairs and indexes, with the stored schema laid over it; exact counts,
//! sampled types, caps, label details, cancellation and the size on the `large` sample graph.
use graphersal::exec::CancelToken;
use graphersal::prelude::*;
use graphersal::schema::{SchemaSummary, SchemaSummaryRequest};

use crate::schema_support::props;

fn summary(graph: &TraversalGraph, request: &SchemaSummaryRequest) -> String {
    graph
        .traversal()
        .schema_summary(request)
        .unwrap()
        .to_string()
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[test]
fn the_modern_graph_in_full() {
    let graph = TraversalGraph::tinkerpop_modern();
    let text = summary(&graph, &SchemaSummaryRequest::new());
    let lines = lines(&text);
    assert_eq!(
        lines[0],
        "graph: 6 vertices, 6 edges; 2 vertex labels, 2 edge labels; no stored schema (mode none)"
    );
    assert!(
        lines[1].starts_with("notation: label count: key type, ...;"),
        "{text}"
    );
    assert_eq!(
        &lines[2..],
        [
            "",
            "vertex labels",
            "person 4: age int, name str",
            "software 2: lang str, name str",
            "",
            "edge labels",
            "created 4 person->software: weight float",
            "knows 2 person->person: weight float",
        ]
    );
    // A small graph is read whole: no sampling note.
    assert!(!text.contains("sampled"), "{text}");
}

#[test]
fn the_stored_schema_is_laid_over_the_data() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let schema = GraphSchema::from_json(
        r#"{"mode": "open",
            "vertices": {
              "person": {"schema": {"description": "A human being.", "type": "object",
                "properties": {"name": {"type": "string", "description": "The given name."},
                               "email": {"type": "string"}}}},
              "software": {"schema": {"type": "object",
                "properties": {"lang": {"type": "string", "enum": ["java", "rust"]}}}},
              "company": {"schema": {"type": "object",
                "properties": {"name": {"type": "string"}}}}},
            "edges": {
              "works_at": {"connections": [{"from": "person", "to": "company"}],
                "schema": {"type": "object"}}}}"#,
    )
    .unwrap();
    graph.traversal_mut().set_schema(schema).unwrap();
    let text = summary(&graph, &SchemaSummaryRequest::new());
    assert!(
        text.contains("schema mode open (stored schema declares 3 vertex labels, 1 edge labels)"),
        "{text}"
    );
    assert!(
        text.contains(
            "person 4: age int, name str, email str (declared, no data)\n  A human being."
        ),
        "{text}"
    );
    assert!(
        text.contains("software 2: lang str in(java|rust), name str"),
        "{text}"
    );
    assert!(
        text.contains("company 0 (declared, no data): name str (declared, no data)"),
        "{text}"
    );
    assert!(
        text.contains("works_at 0 (declared, no data) person->company 0"),
        "{text}"
    );
    // A label's detail carries the key descriptions; descriptions can be switched off.
    let detail = summary(&graph, &SchemaSummaryRequest::new().with_labels(["person"]));
    assert!(detail.contains("name str \"The given name.\""), "{detail}");
    let plain = summary(
        &graph,
        &SchemaSummaryRequest::new().with_descriptions(false),
    );
    assert!(!plain.contains("A human being."), "{plain}");
}

/// A graph with one label `item` of 200 vertices: `a` on all, `b` on 50, `c` on 10, `d` on 1.
fn rare_keys() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    for i in 0..200i64 {
        let mut entries = vec![("a", ElementProperty::from(i))];
        if i < 50 {
            entries.push(("b", ElementProperty::from("x")));
        }
        if i < 10 {
            entries.push(("c", ElementProperty::from(1.5)));
        }
        if i == 0 {
            entries.push((
                "d",
                ElementProperty::Array(vec![ElementProperty::from("t")]),
            ));
        }
        graph
            .add_vertex(None, vec!["item".into()], props(&entries))
            .unwrap();
    }
    graph
}

#[test]
fn keys_not_on_every_element_are_marked_with_their_share() {
    let text = summary(&rare_keys(), &SchemaSummaryRequest::new());
    assert!(
        text.contains("item 200: a int, b str?, c float? 5%, d [str]? n=1"),
        "{text}"
    );
}

#[test]
fn counts_are_exact_and_types_sampled_on_a_bigger_label() {
    let graph = rare_keys();
    let request = SchemaSummaryRequest::new().with_type_samples(5);
    let summary: SchemaSummary = graph.traversal().schema_summary(&request).unwrap();
    assert_eq!(summary.vertex_count(), 200);
    assert!(
        summary.sampled_elements() < 200,
        "{}",
        summary.sampled_elements()
    );
    let text = summary.to_string();
    // Every key keeps its exact count and at least one type (the first element of each key
    // set is always read).
    assert!(
        text.contains("item 200: a int, b str?, c float? 5%, d [str]? n=1"),
        "{text}"
    );
    assert!(
        text.contains("counts are exact, value types sampled from"),
        "{text}"
    );
}

#[test]
fn other_labels_of_a_vertex_and_endpoint_pairs_with_counts() {
    let mut graph = TraversalGraph::new();
    let a = graph
        .add_vertex(None, vec!["person".into(), "admin".into()], props(&[]))
        .unwrap();
    let b = graph
        .add_vertex(None, vec!["person".into()], props(&[]))
        .unwrap();
    let c = graph
        .add_vertex(None, vec!["robot".into()], props(&[]))
        .unwrap();
    for (from, to) in [(a, b), (b, a), (a, c)] {
        graph
            .add_edge(None, Some("knows".into()), from, to, props(&[]))
            .unwrap();
    }
    let text = summary(&graph, &SchemaSummaryRequest::new());
    assert!(text.contains("person 2 (also labelled: admin 1)"), "{text}");
    assert!(
        text.contains("knows 3 person->person 2, person->robot 1"),
        "{text}"
    );
    let detail = summary(&graph, &SchemaSummaryRequest::new().with_labels(["robot"]));
    assert!(detail.contains("edges in: knows<-person 1"), "{detail}");
}

#[test]
fn caps_keep_the_most_frequent_keys_and_the_largest_labels() {
    let mut graph = TraversalGraph::new();
    for label in ["a", "b", "c", "d", "e"] {
        for i in 0..3i64 {
            let mut entries = vec![("shared", ElementProperty::from(i))];
            if i == 0 && matches!(label, "a" | "b") {
                entries.push(("k1", ElementProperty::from(i)));
                entries.push(("k2", ElementProperty::from(i)));
            }
            graph
                .add_vertex(None, vec![label.into()], props(&entries))
                .unwrap();
        }
    }
    // Five labels share `shared`: it is written once.
    let text = summary(&graph, &SchemaSummaryRequest::new());
    assert!(
        text.contains("\nevery vertex label has: shared int\n"),
        "{text}"
    );
    assert!(text.contains("\na 3: k1 int?, k2 int?\n"), "{text}");
    assert!(text.contains("\nc 3\n"), "{text}");
    let capped = summary(
        &graph,
        &SchemaSummaryRequest::new()
            .with_max_keys(1)
            .with_max_labels(2),
    );
    assert!(
        capped.contains("\na 3: k1 int?, ... 1 more keys\n"),
        "{capped}"
    );
    assert!(
        capped.contains("... 3 more vertex labels (9 vertices)"),
        "{capped}"
    );
}

#[test]
fn keys_most_labels_share_are_written_once_with_the_exceptions() {
    let mut graph = TraversalGraph::new();
    for label in ["a", "b", "c", "d", "e"] {
        let mut entries = vec![("other", ElementProperty::from(1i64))];
        if label != "e" {
            entries.push(("service", ElementProperty::from("s")));
        }
        graph
            .add_vertex(None, vec![label.into()], props(&entries))
            .unwrap();
    }
    let text = summary(&graph, &SchemaSummaryRequest::new());
    // `service` on 4 of 5 labels (80 %): written once, e named as the exception; `other` on all.
    assert!(
        text.contains("\nevery vertex label except e has: other int, service str\n"),
        "{text}"
    );
    assert!(text.contains("\na 1\n"), "{text}");
    assert!(text.contains("\ne 1: other int\n"), "{text}");
}

#[cfg(feature = "script")]
#[test]
fn indexes_are_named_in_the_header() {
    let graph = std::sync::Arc::new(GraphSource::tinkerpop_modern());
    crate::script_host::eval_value(
        graph.clone(),
        r#"g.create_index(#{label: "person", key: "name", transform: "to_lower"})"#,
    )
    .unwrap();
    let text = graphersal::storage::AnyGraph::schema_summary(&*graph, &SchemaSummaryRequest::new())
        .unwrap()
        .to_string();
    assert!(text.contains("\nindexes: person.name to_lower\n"), "{text}");
}

#[test]
fn a_cancelled_summary_fails() {
    let graph = GraphSource::large_110k();
    let token = CancelToken::new();
    token.cancel();
    let guard = graph.read();
    let err = guard
        .traversal()
        .schema_summary(&SchemaSummaryRequest::new().with_cancel(token))
        .unwrap_err();
    assert!(matches!(*err, TraverserError::Cancelled { .. }), "{err:?}");
}

#[test]
fn the_large_sample_graph_fits_in_twenty_thousand_characters() {
    let graph = GraphSource::large_110k();
    let guard = graph.read();
    let summary = guard
        .traversal()
        .schema_summary(&SchemaSummaryRequest::new())
        .unwrap();
    let text = summary.to_string();
    assert!(
        text.chars().count() <= 20_000,
        "{} chars",
        text.chars().count()
    );
    assert!(text.starts_with("graph: "), "{text}");
}
