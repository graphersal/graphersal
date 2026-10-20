//! The stage 3a write steps `gql:insert_node` and `gql:insert_edge` on hand-built pipelines
//! (the lowering that builds them is 3a.T4): one element per row, bulk expansion (W15), `NULL`
//! values skipped (W20), value kinds (W21), schema defaults and enforcement (W38), the pinned
//! create time, endpoints from a label, the current object and a row column, endpoint errors,
//! access requests, path requirements and the profile text.

use std::sync::{Arc, Mutex};

use super::*;
use crate::error::{BoxedTraverserError, DiagnosticError};
use crate::gql::eval::compile;
use crate::gql::lexer::Span;
use crate::gql::parser::parse;
use crate::graph::schema::GraphSchema;
use crate::graph::{AccessPolicy, AccessRequest, AccessRequestBuf, Authorizer, Denied};
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::traversal::step::sideeffect_steps::step_gql_insert_edge::GqlEndpoint;
use crate::traversal::step::sideeffect_steps::step_gql_insert_node::{
    GqlInsertProperty, expand_rows,
};
use crate::value::temporal::DateTime;
use crate::{
    __, AccessFlow, GraphError, GraphStep, GraphTraversalSource, PropertyMap, TraversalGraph,
    TraverserResult,
};

/// The values of `MATCH (n) RETURN <items>` as insert properties (each item's alias the key),
/// with `n` the current element and `a` the object labelled `a`; `$p` is `{x: 1}`.
fn props<S: GraphStorage>(items: &str) -> (Arc<str>, Vec<GqlInsertProperty<S>>, Arc<[Binding]>) {
    let text = if items.is_empty() {
        "MATCH (n) RETURN 1".to_string()
    } else {
        format!("MATCH (n) RETURN {items}")
    };
    let parsed = match parse(&text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("{text:?} does not parse: {error}"),
    };
    let names = [CompactString::from("n"), CompactString::from("a")];
    let bindings: Arc<[Binding]> = Arc::from([Binding::Current, Binding::Label("a".into())]);
    let scope = BindingScope {
        names: &names,
        parameter: |name: &str| match name {
            "p" => Some(ElementProperty::Object(Box::new(PropertyMap::from_iter([
                (CompactString::from("x"), ElementProperty::from(1)),
            ])))),
            _ => None,
        },
    };
    let properties = if items.is_empty() {
        Vec::new()
    } else {
        parsed
            .ret
            .items
            .iter()
            .map(|item| GqlInsertProperty {
                key: item
                    .alias
                    .as_ref()
                    .map(|alias| alias.name.as_str().into())
                    .unwrap_or_else(|| panic!("{items:?}: every item needs an alias")),
                value: match compile(&item.expr, &scope) {
                    Ok(expr) => expr,
                    Err(error) => panic!("{items:?} does not compile: {error:?}"),
                },
            })
            .collect()
    };
    (Arc::from(text.as_str()), properties, bindings)
}

fn labels(labels: &[&str]) -> Vec<CompactString> {
    labels
        .iter()
        .map(|label| CompactString::from(*label))
        .collect()
}

/// `start.gql_insert_node(labels, <items>)`.
fn insert<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    node_labels: &[&str],
    items: &str,
) -> GraphTraversalSource<'g, S> {
    let (query, properties, bindings) = props(items);
    start.gql_insert_node(query, labels(node_labels), properties, bindings, Vec::new())
}

/// An endpoint named `name` read through `binding` (span: the first `name` in the query).
fn endpoint(name: &str, binding: Binding) -> GqlEndpoint {
    GqlEndpoint {
        name: name.into(),
        binding,
        span: Span {
            start: 7,
            end: 8,
            line: 1,
            column: 8,
        },
    }
}

/// `start.gql_insert_edge(label, from, to, <items>)`.
fn insert_edge<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    label: &str,
    from: GqlEndpoint,
    to: GqlEndpoint,
    items: &str,
) -> GraphTraversalSource<'g, S> {
    let (query, properties, bindings) = props(items);
    start.gql_insert_edge(
        query,
        label.into(),
        from,
        to,
        properties,
        bindings,
        Vec::new(),
    )
}

/// The property maps of every vertex labelled `label`, in storage order.
fn vertices_of(graph: &TraversalGraph, label: &str) -> Vec<PropertyMap> {
    graph
        .vertices()
        .filter(|v| graph.vertex_labels(*v).iter().any(|l| l == label))
        .map(|v| graph.vertex_properties(v).cloned().unwrap_or_default())
        .collect()
}

/// `(from name, to name, properties)` of every edge labelled `label`.
fn edges_of(graph: &TraversalGraph, label: &str) -> Vec<(String, String, PropertyMap)> {
    let name = |v| {
        graph
            .vertex_properties(v)
            .and_then(|p| p.get("name"))
            .and_then(ElementProperty::as_string)
            .unwrap_or("?")
            .to_string()
    };
    graph
        .edges()
        .filter(|e| graph.edge_label(*e) == Some(label))
        .map(|e| {
            let (from, to) = graph.edge_endpoints(e).unwrap();
            (
                name(from),
                name(to),
                graph.edge_properties(e).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

fn int(value: i64) -> ElementProperty {
    ElementProperty::from(value)
}

/// The error of a run that must fail (its root cause: `.root_cause()`).
fn root(result: TraverserResult<impl std::fmt::Debug>) -> BoxedTraverserError {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error,
    }
}

fn help(error: &TraverserError) -> String {
    error.help().unwrap_or_default()
}

fn counts(graph: &TraversalGraph) -> (usize, usize) {
    (graph.vertices().count(), graph.edges().count())
}

fn gql_status(error: &TraverserError) -> (GqlErrorKind, String, String) {
    match error.root_cause() {
        TraverserError::Gql {
            kind,
            message,
            gqlstatus,
            ..
        } => (*kind, message.clone(), gqlstatus.clone()),
        other => panic!("not a GQL error: {other}"),
    }
}

#[test]
fn insert_node_creates_one_vertex_with_every_label_and_property() {
    let mut graph = TraversalGraph::new();
    let created = insert(
        graph.traversal_mut().inject(vec![0]),
        &["Person", "Admin"],
        "'ann' AS name, 1 + 2 AS n, [1, 'a', [2]] AS mixed, {k: 1, m: {z: true}} AS doc, $p AS p",
    )
    .count()
    .next()
    .unwrap();
    assert_eq!(created, Some(int(1)));
    let v = graph.vertices().next().unwrap();
    assert_eq!(graph.vertex_labels(v), ["Person", "Admin"]);
    let stored = graph.vertex_properties(v).unwrap();
    assert_eq!(stored.get("name"), Some(&ElementProperty::from("ann")));
    assert_eq!(stored.get("n"), Some(&int(3)));
    // W21: maps and mixed / nested lists are stored as they are (a deviation from Neo4j).
    assert_eq!(
        stored.get("mixed"),
        Some(&ElementProperty::Array(vec![
            int(1),
            "a".into(),
            ElementProperty::Array(vec![int(2)])
        ]))
    );
    let doc = match stored.get("doc") {
        Some(ElementProperty::Object(map)) => map,
        other => panic!("not a map: {other:?}"),
    };
    assert_eq!(doc.get("k"), Some(&int(1)));
    assert!(matches!(doc.get("m"), Some(ElementProperty::Object(_))));
    assert!(matches!(stored.get("p"), Some(ElementProperty::Object(_))));
    // A label-less node.
    insert(graph.traversal_mut().inject(vec![0]), &[], "")
        .iterate()
        .unwrap();
    assert_eq!(counts(&graph).0, 2);
}

#[test]
fn insert_node_emits_the_new_vertex_as_a_new_path_position() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // The output is the new vertex, the input stays on the path.
    let paths = insert(
        graph.traversal_mut().v(None).has("name", "marko").as_("a"),
        &["x"],
        "a.name AS of",
    )
    .path()
    .to_list()
    .unwrap();
    assert_eq!(paths.len(), 1);
    match &paths[0] {
        ElementProperty::Array(objects) => assert_eq!(objects.len(), 2, "{paths:?}"),
        other => panic!("not a path: {other:?}"),
    }
    assert_eq!(
        vertices_of(&graph, "x")[0].get("of"),
        Some(&ElementProperty::from("marko"))
    );
}

#[test]
fn every_row_and_every_unit_of_bulk_inserts_one_element() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // Three equal rows: three vertices.
    let out = insert(graph.traversal_mut().inject(vec![1, 1, 1]), &["b"], "")
        .to_list()
        .unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(vertices_of(&graph, "b").len(), 3);
    // lop is reached three times, ripple once: four vertices, whatever the merge option (a
    // mutating plan never merges, e10).
    for merge in [true, false] {
        let before = vertices_of(&graph, "s").len();
        insert(
            graph
                .traversal_mut()
                .with("bulk.merge", merge)
                .v(None)
                .out(None)
                .has_label("software"),
            &["s"],
            "n.name AS name",
        )
        .iterate()
        .unwrap();
        let made = vertices_of(&graph, "s");
        assert_eq!(made.len() - before, 4);
        let lops = made[before..]
            .iter()
            .filter(|p| p.get("name") == Some(&ElementProperty::from("lop")))
            .count();
        assert_eq!(lops, 3);
    }
    // A traverser of bulk 3 is three rows of bulk 1 with its path and sack (W15).
    let mut source: Traverser<TraversalGraph> =
        Traverser::from_value(TraverserValue::Computed(int(7), None));
    source.bulk = 3;
    let rows = expand_rows(source);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.bulk == 1));
    assert!(
        rows.iter()
            .all(|row| row.path == crate::value::PathRef::NONE)
    );
}

#[test]
fn null_values_are_skipped() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    insert(
        graph.traversal_mut().v(None).has("name", "marko"),
        &["x"],
        "1 AS a, NULL AS b, n.missing AS c, [NULL] AS l, {z: NULL} AS m",
    )
    .iterate()
    .unwrap();
    let stored = &vertices_of(&graph, "x")[0];
    // W20: a NULL value stores no key; NULL inside a list or map is part of the value.
    let keys: Vec<&str> = stored.keys().map(CompactString::as_str).collect();
    assert_eq!(keys, ["a", "l", "m"]);
    assert_eq!(
        stored.get("l"),
        Some(&ElementProperty::Array(vec![ElementProperty::Null]))
    );
}

#[test]
fn an_element_is_no_property_value() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    for (items, kind) in [
        ("n AS v", "a node"),
        ("[1, n] AS v", "a node"),
        ("{x: {y: n}} AS v", "a node"),
    ] {
        let error = root(
            insert(
                graph.traversal_mut().v(None).has("name", "marko"),
                &["x"],
                items,
            )
            .iterate(),
        );
        let (kind_of, message, status) = gql_status(&error);
        assert_eq!(kind_of, GqlErrorKind::Data, "{items}");
        assert_eq!(status, "22G03", "{items}");
        assert!(
            message.contains(&format!("property `v` is {kind}")),
            "{items}: {message}"
        );
        assert!(help(&error).contains("element_id(..)"), "{}", help(&error));
    }
    assert!(vertices_of(&graph, "x").is_empty());
}

#[test]
fn an_evaluation_error_fails_the_insert() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let error = root(insert(graph.traversal_mut().inject(vec![0]), &["x"], "1 / 0 AS a").iterate());
    assert_eq!(gql_status(&error).2, "22012");
    assert!(vertices_of(&graph, "x").is_empty());
}

const SCHEMA: &str = r#"{
  "mode": "open",
  "vertices": {
    "item": {"schema": {"type": "object", "required": ["name"], "properties": {
      "name": {"type": "string"},
      "status": {"type": "string", "default": "new"},
      "at": {"type": "string", "format": "date-time", "defaultFunction": "now"}
    }}}
  },
  "edges": {
    "link": {"schema": {"type": "object", "required": ["w"], "properties": {
      "w": {"type": "integer"},
      "kind": {"type": "string", "default": "plain"},
      "at": {"type": "string", "format": "date-time", "defaultFunction": "now"}
    }}}
  }
}"#;

fn schema_graph() -> TraversalGraph {
    let mut graph = TraversalGraph::new();
    let schema = GraphSchema::from_json(SCHEMA).unwrap();
    graph.traversal_mut().set_schema(schema).unwrap();
    graph
}

#[test]
fn schema_defaults_and_required_see_the_whole_element() {
    let mut graph = schema_graph();
    insert(
        graph.traversal_mut().inject(vec![0]),
        &["item"],
        "'a' AS name",
    )
    .iterate()
    .unwrap();
    let stored = &vertices_of(&graph, "item")[0];
    assert_eq!(stored.get("status"), Some(&ElementProperty::from("new")));
    // A given value wins over the default.
    insert(
        graph.traversal_mut().inject(vec![0]),
        &["item"],
        "'b' AS name, 'old' AS status",
    )
    .iterate()
    .unwrap();
    assert_eq!(
        vertices_of(&graph, "item")[1].get("status"),
        Some(&ElementProperty::from("old"))
    );
    // `required` holds because every property arrives in the one create call; a NULL value
    // is no value, so the required key is missing.
    for items in ["", "NULL AS name", "'c' AS status"] {
        let result = insert(graph.traversal_mut().inject(vec![0]), &["item"], items).iterate();
        let error = root(result);
        assert!(
            matches!(
                error.root_cause(),
                TraverserError::Graph(GraphError::Schema(_))
            ),
            "{items}: {error}"
        );
    }
    // A type violation of a value.
    let error = root(
        insert(
            graph.traversal_mut().inject(vec![0]),
            &["item"],
            "1 AS name",
        )
        .iterate(),
    );
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::Graph(GraphError::Schema(_))
        ),
        "{error}"
    );
    assert_eq!(vertices_of(&graph, "item").len(), 2);
    // Edges likewise.
    let from = endpoint("a", Binding::Label("a".into()));
    let to = endpoint("b", Binding::Current);
    let edge = |graph: &mut TraversalGraph, items: &str| {
        let g = graph
            .traversal_mut()
            .v(None)
            .has("name", "a")
            .as_("a")
            .v(None)
            .has("name", "b");
        insert_edge(g, "link", from.clone(), to.clone(), items).iterate()
    };
    edge(&mut graph, "3 AS w").unwrap();
    let (_, _, stored) = &edges_of(&graph, "link")[0];
    assert_eq!(stored.get("kind"), Some(&ElementProperty::from("plain")));
    let error = root(edge(&mut graph, "NULL AS w"));
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::Graph(GraphError::Schema(_))
        ),
        "{error}"
    );
    assert_eq!(edges_of(&graph, "link").len(), 1);
}

#[test]
fn the_create_time_is_pinned_or_one_per_unit() {
    let at: DateTime = "2026-10-15T12:00:00Z".parse().unwrap();
    let later: DateTime = "2027-01-01T00:00:00Z".parse().unwrap();
    let stamp = |p: &PropertyMap| p.get("at").cloned();
    // `with_now` pins the execution's time.
    let mut graph = schema_graph();
    insert(
        graph.traversal_mut().with_now(at).inject(vec![1, 2]),
        &["item"],
        "'a' AS name",
    )
    .iterate()
    .unwrap();
    let made = vertices_of(&graph, "item");
    assert_eq!(made.len(), 2);
    assert!(
        made.iter()
            .all(|p| stamp(p) == Some(ElementProperty::Temporal(at.into())))
    );
    // A time pinned on the step (the script DSL's evaluation `now()`) wins.
    let mut graph = schema_graph();
    let mut pinned = insert(
        graph.traversal_mut().with_now(later).inject(vec![0]),
        &["item"],
        "'a' AS name",
    );
    if let Some(GraphStep::SideEffectMut(_, step)) = pinned.steps.last_mut() {
        step.set_create_time(at);
    }
    let from = endpoint("a", Binding::Label("a".into()));
    let to = endpoint("b", Binding::Current);
    let mut pinned = insert_edge(pinned.as_("a"), "link", from, to, "1 AS w");
    if let Some(GraphStep::SideEffectMut(_, step)) = pinned.steps.last_mut() {
        step.set_create_time(at);
    }
    pinned.iterate().unwrap();
    assert_eq!(
        stamp(&vertices_of(&graph, "item")[0]),
        Some(ElementProperty::Temporal(at.into()))
    );
    assert_eq!(
        stamp(&edges_of(&graph, "link")[0].2),
        Some(ElementProperty::Temporal(at.into()))
    );
    // Unpinned: every element of one unit gets the same time.
    let mut graph = schema_graph();
    insert(
        graph.traversal_mut().inject(vec![1, 2, 3]),
        &["item"],
        "'a' AS name",
    )
    .iterate()
    .unwrap();
    let made = vertices_of(&graph, "item");
    let first = stamp(&made[0]);
    assert!(matches!(first, Some(ElementProperty::Temporal(_))));
    assert!(made.iter().all(|p| stamp(p) == first));
}

/// The modern graph: `josh` labelled `a`, a new `x` vertex the current object.
fn from_label_to_new(graph: &mut TraversalGraph) -> GraphTraversalSource<'_, &mut TraversalGraph> {
    insert(
        graph.traversal_mut().v(None).has("name", "josh").as_("a"),
        &["x"],
        "'new' AS name",
    )
}

#[test]
fn endpoints_from_a_label_the_current_object_and_a_column() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // From a path label to the node inserted just before (the current object).
    let out = insert_edge(
        from_label_to_new(&mut graph),
        "made",
        endpoint("a", Binding::Label("a".into())),
        endpoint("x", Binding::Current),
        "2020 AS since, NULL AS gone",
    )
    .label()
    .to_list()
    .unwrap();
    assert_eq!(out, [ElementProperty::from("made")]);
    let made = edges_of(&graph, "made");
    assert_eq!(made.len(), 1);
    assert_eq!((made[0].0.as_str(), made[0].1.as_str()), ("josh", "new"));
    let keys: Vec<&str> = made[0].2.keys().map(CompactString::as_str).collect();
    assert_eq!(keys, ["since"]);

    // From the column of a `RETURN`-style row (the current record) and from a labelled row.
    let text: Arc<str> = Arc::from("MATCH (n) RETURN n AS p");
    let parsed = parse(&text).unwrap();
    let names = [CompactString::from("n")];
    let scope = BindingScope {
        names: &names,
        parameter: |_: &str| None,
    };
    fn column<S: GraphStorage>(
        parsed: &crate::gql::ast::Query,
        scope: &impl crate::gql::eval::Scope,
    ) -> GqlColumn<S> {
        GqlColumn {
            name: "p".into(),
            expr: compile(&parsed.ret.items[0].expr, scope).unwrap(),
        }
    }
    let rows = graph
        .traversal_mut()
        .v(None)
        .has("name", "vadas")
        .gql_project(
            Arc::clone(&text),
            vec![column(&parsed, &scope)],
            Arc::from([Binding::Current]),
        );
    // Column 0 of the current row, to itself: a self-loop.
    insert_edge(
        rows,
        "self",
        endpoint("p", Binding::Column(0)),
        endpoint("p", Binding::Column(0)),
        "",
    )
    .iterate()
    .unwrap();
    assert_eq!(
        edges_of(&graph, "self")
            .iter()
            .map(|(a, b, _)| (a.as_str(), b.as_str()))
            .collect::<Vec<_>>(),
        [("vadas", "vadas")]
    );
    // A labelled row (`as("#w1")`) read after leaving it.
    let rows = graph
        .traversal_mut()
        .v(None)
        .has("name", "peter")
        .gql_project(
            Arc::clone(&text),
            vec![column(&parsed, &scope)],
            Arc::from([Binding::Current]),
        )
        .as_("#w1");
    let rows = insert(rows, &["y"], "");
    insert_edge(
        rows,
        "to_y",
        endpoint(
            "p",
            Binding::RowColumn {
                label: "#w1".into(),
                column: 0,
            },
        ),
        endpoint("y", Binding::Current),
        "",
    )
    .iterate()
    .unwrap();
    let to_y = edges_of(&graph, "to_y");
    assert_eq!(to_y.len(), 1);
    assert_eq!(to_y[0].0, "peter");
}

#[test]
fn a_null_removed_or_non_node_endpoint_is_an_error() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = counts(&graph);
    // NULL: the label is not on the path (an OPTIONAL MATCH that did not match).
    let error = root(
        insert_edge(
            from_label_to_new(&mut graph),
            "r",
            endpoint("m", Binding::Label("missing".into())),
            endpoint("x", Binding::Current),
            "",
        )
        .iterate(),
    );
    let (kind, message, status) = gql_status(&error);
    assert_eq!((kind, status.as_str()), (GqlErrorKind::Data, "22004"));
    assert_eq!(
        message,
        "the source node `m` of the inserted `:r` edge is NULL"
    );
    assert!(
        help(&error).contains("FILTER m IS NOT NULL"),
        "{}",
        help(&error)
    );
    // The destination likewise.
    let error = root(
        insert_edge(
            from_label_to_new(&mut graph),
            "r",
            endpoint("a", Binding::Label("a".into())),
            endpoint("m", Binding::Label("missing".into())),
            "",
        )
        .iterate(),
    );
    assert_eq!(
        gql_status(&error).1,
        "the destination node `m` of the inserted `:r` edge is NULL"
    );
    // Not a node: an edge or a scalar.
    let error = root(
        insert_edge(
            graph
                .traversal_mut()
                .v(None)
                .has("name", "marko")
                .out_e(None)
                .limit(1),
            "r",
            endpoint("e", Binding::Current),
            endpoint("e", Binding::Current),
            "",
        )
        .iterate(),
    );
    let (_, message, status) = gql_status(&error);
    assert_eq!(status, "22G03");
    assert!(message.ends_with("is not a node"), "{message}");
    // Removed earlier in the query: the storage's error, as for `add_e`.
    let error = root(
        insert_edge(
            graph
                .traversal_mut()
                .v(None)
                .has("name", "marko")
                .as_("a")
                .side_effect_t(__::drop()),
            "r",
            endpoint("a", Binding::Label("a".into())),
            endpoint("a", Binding::Label("a".into())),
            "",
        )
        .iterate(),
    );
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::Graph(GraphError::ElementRemoved { .. })
        ),
        "{error}"
    );
    // Nothing of a failed traversal stays.
    assert_eq!(counts(&graph), before);
}

#[test]
fn a_read_source_refuses_the_insert_like_add_v() {
    let graph = TraversalGraph::tinkerpop_modern();
    let gql = root(insert(graph.traversal().inject(vec![0]), &["x"], "").iterate());
    let gremlin = root(graph.traversal().inject(vec![0]).add_v("x").iterate());
    assert_eq!(
        gql.root_cause().to_string(),
        gremlin.root_cause().to_string()
    );
    assert_eq!(help(&gql), help(&gremlin));
    assert!(
        gql.root_cause()
            .to_string()
            .contains("Cannot mutate immutable graph reference"),
        "{gql}"
    );
}

/// An authorizer recording every request, allowing all but `Create` on `deny_label`.
struct Recorder {
    seen: Mutex<Vec<AccessRequestBuf>>,
    deny_label: Option<&'static str>,
}

impl Authorizer for Recorder {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(request.to_owned_request());
        }
        match (self.deny_label, request.resource) {
            (Some(denied), crate::graph::Resource::Data { label, .. })
                if label == Some(denied)
                    && matches!(request.action, crate::graph::Action::Create) =>
            {
                Err(Denied::new("secret"))
            }
            _ => Ok(()),
        }
    }
}

fn recorder(deny_label: Option<&'static str>) -> Arc<Recorder> {
    Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
        deny_label,
    })
}

fn seen(recorder: &Recorder) -> Vec<String> {
    recorder
        .seen
        .lock()
        .map(|seen| seen.iter().map(ToString::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn inserts_ask_what_add_v_and_add_e_ask() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // Nodes: one `Create` per label, label-less as `add_v()`.
    for node_labels in [&["person", "admin"][..], &["x"][..], &[][..]] {
        let gql = recorder(None);
        insert(
            graph
                .traversal_mut()
                .with_authorizer(gql.clone())
                .inject(vec![0]),
            node_labels,
            "1 AS a",
        )
        .iterate()
        .unwrap();
        let gremlin = recorder(None);
        let add_v = match node_labels {
            [] => graph
                .traversal_mut()
                .with_authorizer(gremlin.clone())
                .inject(vec![0])
                .add_v(None),
            many => graph
                .traversal_mut()
                .with_authorizer(gremlin.clone())
                .inject(vec![0])
                .add_v(many.iter().map(|l| l.to_string()).collect::<Vec<_>>()),
        };
        add_v.iterate().unwrap();
        // The properties are part of the create (one `add_vertex_with` call, like a folded
        // `add_v(..).property(..)`): no `Update`, which the unfolded Gremlin twin also asks.
        assert_eq!(seen(&gql), seen(&gremlin), "{node_labels:?}");
        assert!(
            seen(&gql).iter().any(|r| r.starts_with("Create")),
            "{:?}",
            seen(&gql)
        );
    }
    // Edges: `Create` on the edge label, nothing for the endpoints (bound by earlier steps).
    let gql = recorder(None);
    insert_edge(
        graph
            .traversal_mut()
            .with_authorizer(gql.clone())
            .v(None)
            .has("name", "marko")
            .as_("a")
            .out(None),
        "likes",
        endpoint("a", Binding::Label("a".into())),
        endpoint("b", Binding::Current),
        "",
    )
    .iterate()
    .unwrap();
    let gremlin = recorder(None);
    graph
        .traversal_mut()
        .with_authorizer(gremlin.clone())
        .v(None)
        .has("name", "marko")
        .as_("a")
        .out(None)
        .add_e("likes")
        .from("a")
        .iterate()
        .unwrap();
    assert_eq!(seen(&gql), seen(&gremlin));
    // A step after an insert sees the inserted element's kind and labels.
    let steps = insert(GraphTraversalSource::new(&graph), &["person"], "").take_steps();
    let flow = steps[0].access_flow_after(AccessFlow::default());
    assert_eq!(flow.element, Some(ElementKind::Vertex));
    assert_eq!(flow.labels, ["person"]);
    assert!(steps[0].mutates());
}

#[test]
fn a_denied_insert_changes_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // Label-level: `INSERT (:secret)` is denied before anything runs.
    let denied = recorder(Some("secret"));
    let error = root(
        insert(
            graph
                .traversal_mut()
                .with_authorizer(denied.clone())
                .inject(vec![0]),
            &["public", "secret"],
            "",
        )
        .iterate(),
    );
    assert!(
        matches!(error.root_cause(), TraverserError::PermissionDenied { .. }),
        "{error}"
    );
    let error = root(
        insert_edge(
            graph
                .traversal_mut()
                .with_authorizer(denied.clone())
                .v(None)
                .as_("a"),
            "secret",
            endpoint("a", Binding::Label("a".into())),
            endpoint("a", Binding::Current),
            "",
        )
        .iterate(),
    );
    assert!(
        matches!(error.root_cause(), TraverserError::PermissionDenied { .. }),
        "{error}"
    );
    // A read-only policy refuses every insert.
    let error = root(
        insert(
            graph
                .traversal_mut()
                .with_authorizer(Arc::new(AccessPolicy::read_only()))
                .inject(vec![0]),
            &["x"],
            "",
        )
        .iterate(),
    );
    assert!(
        matches!(error.root_cause(), TraverserError::PermissionDenied { .. }),
        "{error}"
    );
    assert_eq!(counts(&graph), (6, 6));
}

#[test]
fn profile_text_and_path_requirements() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let node = insert(
        graph.traversal_mut().v(None).has("name", "marko").as_("a"),
        &["Person", "Admin"],
        "a.name AS name, NULL AS gone",
    );
    let profile = insert_edge(
        node,
        "knows",
        endpoint("a", Binding::Label("a".into())),
        endpoint("n", Binding::Current),
        "2020 AS since",
    )
    .profile()
    .unwrap();
    let names: Vec<String> = profile
        .metrics()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    assert!(
        names.contains(
            &r#"gql:insert_node(["Person", "Admin"], "{name: a.name, gone: NULL}")"#.to_string()
        ),
        "{profile}"
    );
    assert!(
        names.contains(&r#"gql:insert_edge("knows", "a", "n", "{since: 2020}")"#.to_string()),
        "{profile}"
    );
    // Without properties.
    let name = insert(GraphTraversalSource::new(&graph), &[], "").take_steps()[0].get_name();
    assert_eq!(name, "gql:insert_node([])");
    // The edge reads the label `a` (rule 13), so the path keeps it up to the edge: the profile
    // shows it on the node insert before it.
    let text = profile.to_string();
    assert!(text.contains("[path: labels(a)]"), "{text}");
    // Under camel case the GQL text in the literals is not respelled.
    let camel = insert(
        graph
            .traversal_mut()
            .with("render.spelling", "camel")
            .inject(vec![0]),
        &["x"],
        "char_length('a') AS name",
    )
    .profile()
    .unwrap()
    .to_string();
    // The step name is respelled like every step name (`gql:not_self_loop` likewise).
    assert!(camel.contains("gql:insertNode("), "{camel}");
    assert!(camel.contains("char_length('a')"), "{camel}");
}
