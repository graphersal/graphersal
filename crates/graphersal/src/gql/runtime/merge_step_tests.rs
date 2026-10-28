//! The stage 3e write step `gql:merge` on hand-built match and create children (the lowering
//! builds them since 3e.T3): idempotence, rows seeing earlier rows' creations (W34), several
//! matches as several rows (W31), `ON CREATE` vs `ON MATCH` (W35), the run-time `NULL` check
//! (E1, 22004), bulk expansion (W15), schema defaults on create (W37), the pinned create time,
//! rollback of a failing later row, access requests (C7), path requirements and the profile.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::*;
use crate::error::{BoxedTraverserError, DiagnosticError};
use crate::gql::eval::compile;
use crate::gql::lexer::Span;
use crate::gql::parser::parse;
use crate::graph::schema::GraphSchema;
use crate::graph::{AccessRequest, AccessRequestBuf, Authorizer, Denied};
use crate::traversal::TraversalStrategy;
use crate::traversal::optimizer::path_analysis::PathRequirement;
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::traversal::step::sideeffect_steps::step_gql_insert_node::GqlInsertProperty;
use crate::traversal::step::sideeffect_steps::step_gql_merge::GqlMerge;
use crate::traversal::step::sideeffect_steps::step_gql_set::{GqlSetItem, GqlWriteTarget};
use crate::{
    AccessFlow, AnonymousTraversal, GraphStep, GraphTraversalSource, PropertyMap, TraversalGraph,
    TraverserResult,
};

/// `MERGE (n:<labels> {<key>: <value>}) [ON CREATE SET n.<k> = <e>, ..] [ON MATCH SET ..]`,
/// with `x` the object the row path labels `x` (`NULL` when the row has no such label).
#[derive(Clone, Copy)]
struct Merge {
    labels: &'static [&'static str],
    key: &'static str,
    value: &'static str,
    on_create: &'static [(&'static str, &'static str)],
    on_match: &'static [(&'static str, &'static str)],
}

const N: Merge = Merge {
    labels: &["N"],
    key: "k",
    value: "x",
    on_create: &[],
    on_match: &[],
};

const NAMES: [&str; 2] = ["n", "x"];

fn names() -> Vec<CompactString> {
    NAMES.iter().map(|n| CompactString::from(*n)).collect()
}

fn span_of(query: &str, text: &str, from: usize) -> Span {
    let start = query[from..]
        .find(text)
        .map(|at| at + from)
        .unwrap_or_else(|| panic!("{text:?} not in {query:?}"));
    Span {
        start,
        end: start + text.len(),
        line: 1,
        column: start + 1,
    }
}

/// `n` (slot 0) and `x` (slot 1, the row label) read through `n`.
fn slots(n: Binding) -> Arc<[Binding]> {
    Arc::from([n, Binding::Label("x".into())])
}

/// The `gql:merge` parts of `m`: the query is `MATCH (n) RETURN <every expression>` followed by
/// the MERGE clause in a comment (the spans of the pattern and the items point into it).
fn parts<'g, S: GraphStorage + 'g>(m: Merge) -> GqlMerge<'g, S> {
    let mut items = vec![
        format!("{} AS c0", m.value),
        format!("n.{} = {} AS c1", m.key, m.value),
        "n AS n".to_string(),
    ];
    for (i, (_, expr)) in m.on_create.iter().chain(m.on_match).enumerate() {
        items.push(format!("{expr} AS o{i}"));
    }
    let labels: String = m.labels.iter().map(|l| format!(":{l}")).collect();
    let pattern = format!("(n{labels} {{{}: {}}})", m.key, m.value);
    let mut clause = pattern.clone();
    let item_text = |(key, expr): &(&str, &str)| format!("n.{key} = {expr}");
    let texts = |list: &[(&str, &str)]| list.iter().map(item_text).collect::<Vec<_>>();
    if !m.on_create.is_empty() {
        clause.push_str(&format!(" ON CREATE SET {}", texts(m.on_create).join(", ")));
    }
    if !m.on_match.is_empty() {
        clause.push_str(&format!(" ON MATCH SET {}", texts(m.on_match).join(", ")));
    }
    let text = format!("MATCH (n) RETURN {}\n-- MERGE {clause}", items.join(", "));
    let parsed = match parse(&text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("{text:?} does not parse: {error}"),
    };
    let query: Arc<str> = Arc::from(text.as_str());
    let names = names();
    let scope = BindingScope {
        names: &names,
        parameter: |_: &str| None,
    };
    let expr = |i: usize| -> RowExpr<S> {
        match compile(&parsed.ret.items[i].expr, &scope) {
            Ok(expr) => expr,
            Err(error) => panic!("item {i} of {text:?} does not compile: {error:?}"),
        }
    };
    let record = || -> Vec<GqlColumn<S>> {
        vec![GqlColumn {
            name: "n".into(),
            expr: expr(2),
        }]
    };
    let node_labels: Vec<CompactString> = m.labels.iter().map(|l| (*l).into()).collect();
    let property = || GqlInsertProperty {
        key: m.key.into(),
        value: expr(0),
    };
    let matcher = AnonymousTraversal::new()
        .v(None)
        .has_label(m.labels.iter().map(|l| l.to_string()).collect::<Vec<_>>())
        .gql_filter(Arc::clone(&query), expr(1), slots(Binding::Current))
        .gql_project_hosted(
            Arc::clone(&query),
            record(),
            slots(Binding::Current),
            Vec::new(),
        );
    let creator = AnonymousTraversal::new()
        .gql_insert_node(
            Arc::clone(&query),
            node_labels.clone(),
            vec![property()],
            slots(Binding::Current),
            Vec::new(),
        )
        .gql_project_hosted(
            Arc::clone(&query),
            record(),
            slots(Binding::Current),
            Vec::new(),
        );
    let marker = text.find("-- MERGE").unwrap_or_default();
    let mut from = marker;
    let mut set_items = |list: &[(&str, &str)], first: usize| -> Vec<GqlSetItem<S>> {
        let mut out = Vec::new();
        for (offset, item) in list.iter().enumerate() {
            let span = span_of(&text, &item_text(item), from);
            from = span.end;
            out.push(GqlSetItem::Property {
                target: GqlWriteTarget {
                    name: "n".into(),
                    binding: Binding::Column(0),
                    span: span_of(&text, "n", span.start),
                    element: Some(ElementKind::Vertex),
                    labels: Vec::new(),
                },
                key: item.0.into(),
                value: expr(3 + first + offset),
                span,
            });
        }
        out
    };
    let on_create = set_items(m.on_create, 0);
    let on_match = set_items(m.on_match, m.on_create.len());
    let creates = if node_labels.is_empty() {
        vec![(ElementKind::Vertex, None)]
    } else {
        node_labels
            .iter()
            .map(|l| (ElementKind::Vertex, Some(l.clone())))
            .collect()
    };
    GqlMerge {
        pattern: span_of(&text, &pattern, marker),
        matcher,
        creator,
        checks: vec![property()],
        check_bindings: slots(Binding::Label("#none".into())),
        on_create,
        on_match,
        item_bindings: slots(Binding::Column(0)),
        item_children: Vec::new(),
        creates,
        query,
    }
}

fn merge<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    m: Merge,
) -> GraphTraversalSource<'g, S> {
    start.gql_merge(parts(m))
}

/// The rows `values`, each labelled `x`.
fn rows<'g>(
    graph: &'g mut TraversalGraph,
    values: Vec<i64>,
) -> GraphTraversalSource<'g, &'g mut TraversalGraph> {
    graph.traversal_mut().inject(values).as_("x")
}

/// The property maps of every vertex labelled `label`, in storage order.
fn vertices_of(graph: &TraversalGraph, label: &str) -> Vec<PropertyMap> {
    graph
        .vertices()
        .unwrap()
        .filter(|v| graph.vertex_labels(*v).unwrap().iter().any(|l| l == label))
        .map(|v| {
            graph
                .vertex_properties(v)
                .unwrap()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// The values of `key` of the vertices labelled `label`, in storage order.
fn values_of(graph: &TraversalGraph, label: &str, key: &str) -> Vec<Option<ElementProperty>> {
    vertices_of(graph, label)
        .iter()
        .map(|p| p.get(key).cloned())
        .collect()
}

fn int(value: i64) -> ElementProperty {
    ElementProperty::from(value)
}

fn root(result: TraverserResult<impl std::fmt::Debug>) -> BoxedTraverserError {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error,
    }
}

fn gql_status(error: &TraverserError) -> String {
    match error.root_cause() {
        TraverserError::Gql { gqlstatus, .. } => gqlstatus.clone(),
        other => panic!("not a GQL error: {other}"),
    }
}

#[test]
fn merge_twice_is_merge_once() {
    let mut graph = TraversalGraph::new();
    merge(rows(&mut graph, vec![1]), N).iterate().unwrap();
    assert_eq!(values_of(&graph, "N", "k"), [Some(int(1))]);
    let out = merge(rows(&mut graph, vec![1]), N).to_list().unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(values_of(&graph, "N", "k"), [Some(int(1))]);
    // A different value creates.
    merge(rows(&mut graph, vec![2]), N).iterate().unwrap();
    assert_eq!(values_of(&graph, "N", "k"), [Some(int(1)), Some(int(2))]);
}

#[test]
fn every_row_sees_what_the_rows_before_it_created() {
    // `FOR x IN [1, 1, 2] MERGE (n:N {k: x})` creates two nodes (W34) and gives three rows.
    let mut graph = TraversalGraph::new();
    let out = merge(rows(&mut graph, vec![1, 1, 2]), N).to_list().unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(values_of(&graph, "N", "k"), [Some(int(1)), Some(int(2))]);
    // Each output is the record row `{n: <node>}`.
    for row in &out {
        let ElementProperty::Object(map) = row else {
            panic!("not a record: {row:?}");
        };
        assert!(map.get("n").is_some(), "{row:?}");
    }
}

#[test]
fn several_matches_are_several_rows_with_on_match_on_each() {
    let mut graph = TraversalGraph::new();
    let mut make = graph.traversal_mut().inject(vec![0]);
    make = make.add_v("N").property("k", 1).add_v("N").property("k", 1);
    make.iterate().unwrap();
    let counting = Merge {
        on_create: &[("created", "1")],
        on_match: &[("hits", "coalesce(n.hits, 0) + 1")],
        ..N
    };
    let out = merge(rows(&mut graph, vec![1]), counting)
        .to_list()
        .unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(values_of(&graph, "N", "hits"), [Some(int(1)), Some(int(1))]);
    assert_eq!(values_of(&graph, "N", "created"), [None, None]);
}

#[test]
fn on_create_runs_on_the_created_record_and_on_match_on_later_matches() {
    let mut graph = TraversalGraph::new();
    let counting = Merge {
        on_create: &[("created", "1"), ("seen", "x * 10")],
        on_match: &[("hits", "coalesce(n.hits, 0) + 1")],
        ..N
    };
    // Row 1 creates (ON CREATE), rows 2 and 3 match the creation (ON MATCH, W34).
    merge(rows(&mut graph, vec![5, 5, 5]), counting)
        .iterate()
        .unwrap();
    let stored = vertices_of(&graph, "N");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].get("created"), Some(&int(1)));
    assert_eq!(stored[0].get("seen"), Some(&int(50)));
    assert_eq!(stored[0].get("hits"), Some(&int(2)));
    // A NULL value removes, as in SET.
    let removing = Merge {
        on_match: &[("seen", "NULL")],
        ..N
    };
    merge(rows(&mut graph, vec![5]), removing)
        .iterate()
        .unwrap();
    assert_eq!(vertices_of(&graph, "N")[0].get("seen"), None);
}

#[test]
fn a_null_property_value_is_22004_before_anything_runs() {
    let mut graph = TraversalGraph::new();
    // The row has no label `x`: `x` is NULL.
    let error = root(merge(graph.traversal_mut().inject(vec![1]), N).iterate());
    let TraverserError::Gql {
        kind,
        message,
        gqlstatus,
        ..
    } = error.root_cause()
    else {
        panic!("not a GQL error: {error}");
    };
    assert_eq!(*kind, GqlErrorKind::Data);
    assert_eq!(gqlstatus, "22004");
    assert_eq!(
        message,
        "MERGE cannot match or create `{k: NULL}`: a property value is NULL"
    );
    let help = error.help().unwrap_or_default();
    assert!(help.contains("FILTER x IS NOT NULL"), "{help}");
    assert!(help.contains("coalesce(x, 0)"), "{help}");
    assert!(graph.vertices().unwrap().next().is_none());
}

#[test]
fn a_failing_later_row_leaves_nothing() {
    let mut graph = TraversalGraph::new();
    let failing = Merge {
        value: "CASE WHEN x = 3 THEN NULL ELSE x END",
        ..N
    };
    let error = root(merge(rows(&mut graph, vec![1, 2, 3]), failing).iterate());
    assert_eq!(gql_status(&error), "22004");
    assert!(
        graph.vertices().unwrap().next().is_none(),
        "rows 1 and 2 were kept"
    );
    // An error of an ON item fails it the same way.
    let dividing = Merge {
        on_match: &[("q", "1 / 0")],
        ..N
    };
    let error = root(merge(rows(&mut graph, vec![1, 1]), dividing).iterate());
    assert_eq!(gql_status(&error), "22012");
    assert!(graph.vertices().unwrap().next().is_none());
}

#[test]
fn a_row_of_bulk_n_is_n_rows() {
    // A constant pattern: the first unit creates, the two others match it (W15 + W34).
    let constant = Merge {
        labels: &["B"],
        value: "1",
        on_match: &[("hits", "coalesce(n.hits, 0) + 1")],
        ..N
    };
    let mut builder = TraversalGraph::new();
    let mut graph = TraversalGraph::new();
    {
        let steps = merge(builder.traversal_mut(), constant).take_steps();
        let Some(GraphStep::SideEffectMut(_, step)) = steps.first() else {
            panic!("not a SideEffectMut");
        };
        let mut storage = &mut graph;
        let mut context =
            TraverserContext::new(&mut storage, TraversalStrategy::none(), HashMap::new());
        let mut source = Traverser::from_value(TraverserValue::Computed(int(0), None));
        source.bulk = 3;
        let out = step.process(&mut context, Some(source)).unwrap();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|row| row.bulk == 1));
    }
    assert_eq!(values_of(&graph, "B", "hits"), [Some(int(2))]);
}

const SCHEMA: &str = r#"{
  "mode": "open",
  "vertices": {
    "item": {"schema": {"type": "object", "required": ["k"], "properties": {
      "k": {"type": "integer"},
      "status": {"type": "string", "default": "new"},
      "at": {"type": "string", "format": "date-time", "defaultFunction": "now"}
    }}}
  }
}"#;

#[test]
fn a_created_element_gets_the_schema_defaults_and_the_pinned_time() {
    let mut graph = TraversalGraph::new();
    graph
        .traversal_mut()
        .set_schema(GraphSchema::from_json(SCHEMA).unwrap())
        .unwrap();
    let item = Merge {
        labels: &["item"],
        ..N
    };
    merge(rows(&mut graph, vec![1]), item).iterate().unwrap();
    let stored = &vertices_of(&graph, "item")[0];
    assert_eq!(stored.get("status"), Some(&ElementProperty::from("new")));
    assert!(matches!(
        stored.get("at"),
        Some(ElementProperty::Temporal(_))
    ));
    // A time pinned on the step reaches the creator's insert step.
    let at: crate::value::temporal::DateTime = "2024-05-06T07:08:09Z".parse().unwrap();
    let mut pinned = merge(rows(&mut graph, vec![2]), item);
    if let Some(GraphStep::SideEffectMut(_, step)) = pinned.steps.last_mut() {
        step.set_create_time(at);
    }
    pinned.iterate().unwrap();
    assert_eq!(
        vertices_of(&graph, "item")[1].get("at"),
        Some(&ElementProperty::Temporal(at.into()))
    );
}

/// An authorizer recording every request, allowing all.
struct Recorder {
    seen: Mutex<Vec<AccessRequestBuf>>,
}

impl Authorizer for Recorder {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(request.to_owned_request());
        }
        Ok(())
    }
}

fn recorded(m: Merge) -> Vec<String> {
    let mut graph = TraversalGraph::new();
    let recorder = Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
    });
    merge(
        graph
            .traversal_mut()
            .with_authorizer(recorder.clone())
            .inject(vec![1])
            .as_("x"),
        m,
    )
    .iterate()
    .unwrap();
    let mut seen: Vec<String> = recorder
        .seen
        .lock()
        .map(|seen| seen.iter().map(ToString::to_string).collect())
        .unwrap_or_default();
    seen.sort();
    seen.dedup();
    seen
}

#[test]
fn merge_asks_read_create_and_update_only_with_on_items() {
    let plain = recorded(N);
    assert!(plain.iter().any(|r| r.starts_with("Read")), "{plain:?}");
    assert!(
        plain
            .iter()
            .any(|r| r.starts_with("Create") && r.contains("N")),
        "{plain:?}"
    );
    assert!(!plain.iter().any(|r| r.starts_with("Update")), "{plain:?}");
    let with_items = recorded(Merge {
        on_create: &[("a", "1")],
        ..N
    });
    assert!(
        with_items.iter().any(|r| r.starts_with("Update")),
        "{with_items:?}"
    );
    // The step's own requests (children not included), its flow and `mutates()`.
    let builder = TraversalGraph::new();
    let steps = merge(
        GraphTraversalSource::new(&builder),
        Merge {
            labels: &["A", "B"],
            on_match: &[("hits", "1")],
            ..N
        },
    )
    .take_steps();
    let mut own = Vec::new();
    steps[0]
        .for_each_access(|request| {
            own.push(request.to_owned_request().to_string());
            Ok::<(), ()>(())
        })
        .unwrap();
    assert_eq!(own.len(), 3, "{own:?}");
    assert!(
        own[0].starts_with("Create") && own[0].contains('A'),
        "{own:?}"
    );
    assert!(
        own[1].starts_with("Create") && own[1].contains('B'),
        "{own:?}"
    );
    assert!(own[2].starts_with("Update"), "{own:?}");
    assert!(steps[0].mutates());
    let flow = AccessFlow {
        element: Some(ElementKind::Vertex),
        labels: vec!["person".into()],
    };
    let after = steps[0].access_flow_after(flow);
    assert_eq!(after.element, None);
    assert!(after.labels.is_empty());
}

#[test]
fn profile_shows_both_children_and_path_requirements() {
    let mut graph = TraversalGraph::new();
    let profile = merge(
        rows(&mut graph, vec![1]),
        Merge {
            on_create: &[("a", "1")],
            on_match: &[("b", "x")],
            ..N
        },
    )
    .profile()
    .unwrap();
    let names: Vec<String> = profile
        .metrics()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let step = names
        .iter()
        .find(|name| name.starts_with("gql:merge("))
        .unwrap_or_else(|| panic!("{profile}"));
    assert!(
        step.starts_with(r#"gql:merge("(n:N {k: x})", __.v("#),
        "{step}"
    );
    assert!(step.contains("gql:insert_node("), "{step}");
    assert!(
        step.ends_with(r#""ON CREATE SET n.a = 1", "ON MATCH SET n.b = x")"#),
        "{step}"
    );
    // The check and the ON MATCH value read `x` (rule 13).
    let builder = TraversalGraph::new();
    let steps = merge(GraphTraversalSource::new(&builder), N).take_steps();
    let GraphStep::SideEffectMut(_, step) = &steps[0] else {
        panic!("not a SideEffectMut");
    };
    assert_eq!(
        step.path_requirement(),
        PathRequirement::labels([CompactString::from("x")])
    );
    let text = profile.to_string();
    assert!(text.contains("labels(x)"), "{text}");
}
