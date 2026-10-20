//! The stage 3c write steps `gql:set` and `gql:remove` on hand-built pipelines (built by the
//! lowering since 3c.T3): property items (a `NULL` value removes, W20), `=` replace (W23,
//! atomic per item) and `+=` merge (W24), label items (W26), `NULL` targets (W25), removed
//! targets (W27), non-element targets, schema coercion and enforcement through the storage
//! write path, bulk expansion (W15), access requests equal to the Gremlin twins, path
//! requirements and the profile text.

use std::sync::{Arc, Mutex};

use super::*;
use crate::error::{BoxedTraverserError, DiagnosticError};
use crate::gql::eval::compile;
use crate::gql::lexer::Span;
use crate::gql::parser::parse;
use crate::graph::schema::GraphSchema;
use crate::graph::{AccessPolicy, AccessRequest, AccessRequestBuf, Authorizer, Denied};
use crate::traversal::step::sideeffect_steps::step_gql_remove::GqlRemoveItem;
use crate::traversal::step::sideeffect_steps::step_gql_set::{GqlSetItem, GqlWriteTarget};
use crate::{
    __, AccessFlow, AnonymousTraversal, GraphError, GraphStep, GraphTraversalSource, PropertyMap,
    TraversalGraph, TraverserResult, VertexHandle,
};

/// An item as the tests write it: the target variable (`n` the current object, `a` the object
/// labelled `a`, `x` a label never set: `NULL`) and its parts.
#[derive(Clone, Copy)]
enum It {
    /// `t.key = expr`
    Prop(&'static str, &'static str, &'static str),
    /// `t = expr`
    Replace(&'static str, &'static str),
    /// `t += expr`
    Merge(&'static str, &'static str),
    /// `t:A:B` (SET) / `t:A:B` (REMOVE)
    Labels(&'static str, &'static [&'static str]),
    /// `t.key` (REMOVE)
    Key(&'static str, &'static str),
}

impl It {
    fn text(self) -> String {
        match self {
            It::Prop(t, key, expr) => format!("{t}.{key} = {expr}"),
            It::Replace(t, expr) => format!("{t} = {expr}"),
            It::Merge(t, expr) => format!("{t} += {expr}"),
            It::Labels(t, labels) => format!("{t}:{}", labels.join(":")),
            It::Key(t, key) => format!("{t}.{key}"),
        }
    }

    fn target(self) -> &'static str {
        match self {
            It::Prop(t, ..)
            | It::Replace(t, _)
            | It::Merge(t, _)
            | It::Labels(t, _)
            | It::Key(t, _) => t,
        }
    }

    fn expr(self) -> Option<&'static str> {
        match self {
            It::Prop(_, _, expr) | It::Replace(_, expr) | It::Merge(_, expr) => Some(expr),
            It::Labels(..) | It::Key(..) => None,
        }
    }
}

const NAMES: [&str; 3] = ["n", "a", "x"];

fn bindings() -> Arc<[Binding]> {
    Arc::from([
        Binding::Current,
        Binding::Label("a".into()),
        Binding::Label("x".into()),
    ])
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

/// The query text of `items`: the values as `RETURN` items, then the item texts in a comment
/// (the spans of the profile text and the targets point into it).
fn build(items: &[It]) -> (Arc<str>, crate::gql::ast::Query, usize) {
    let values: Vec<String> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| item.expr().map(|expr| format!("{expr} AS v{i}")))
        .collect();
    let mut text = String::from("MATCH (n) RETURN ");
    text.push_str(&if values.is_empty() {
        "1 AS one".to_string()
    } else {
        values.join(", ")
    });
    text.push_str("\n-- ");
    let marker = text.len();
    let texts: Vec<String> = items.iter().map(|item| item.text()).collect();
    text.push_str(&texts.join(", "));
    let parsed = match parse(&text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("{text:?} does not parse: {error}"),
    };
    (Arc::from(text.as_str()), parsed, marker)
}

fn target(
    query: &str,
    item: It,
    at: Span,
    element: Option<ElementKind>,
    labels: &[&str],
) -> GqlWriteTarget {
    let name = item.target();
    let slot = NAMES.iter().position(|n| *n == name).unwrap();
    GqlWriteTarget {
        name: name.into(),
        binding: bindings()[slot].clone(),
        span: span_of(query, name, at.start),
        element,
        labels: labels.iter().map(|l| CompactString::from(*l)).collect(),
    }
}

fn names() -> Vec<CompactString> {
    NAMES.iter().map(|n| CompactString::from(*n)).collect()
}

fn param(name: &str) -> Option<ElementProperty> {
    match name {
        "p" => Some(ElementProperty::Object(Box::new(PropertyMap::from_iter([
            (CompactString::from("k"), ElementProperty::from(1)),
            (CompactString::from("gone"), ElementProperty::Null),
        ])))),
        _ => None,
    }
}

/// `start.gql_set(items)`, every target of the kind `element` with the edge labels `labels`.
fn set_with<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    items: &[It],
    element: Option<ElementKind>,
    labels: &[&str],
) -> GraphTraversalSource<'g, S> {
    let (query, out) = set_items(items, element, labels);
    start.gql_set(query, out, bindings(), Vec::new())
}

/// The query text and the `SET` items of `items`.
fn set_items<S: GraphStorage>(
    items: &[It],
    element: Option<ElementKind>,
    labels: &[&str],
) -> (Arc<str>, Vec<GqlSetItem<S>>) {
    let (query, parsed, marker) = build(items);
    let names = names();
    let scope = BindingScope {
        names: &names,
        parameter: param,
    };
    let mut values = parsed.ret.items.iter();
    let mut from = marker;
    let mut out = Vec::new();
    for item in items {
        let span = span_of(&query, &item.text(), from);
        from = span.end;
        let target = target(&query, *item, span, element, labels);
        let mut value = || match item.expr() {
            Some(_) => {
                let returned = values.next().unwrap();
                match compile(&returned.expr, &scope) {
                    Ok(expr) => expr,
                    Err(error) => panic!("{:?} does not compile: {error:?}", item.text()),
                }
            }
            None => unreachable!(),
        };
        out.push(match *item {
            It::Prop(_, key, _) => GqlSetItem::Property {
                target,
                key: key.into(),
                value: value(),
                span,
            },
            It::Replace(..) => GqlSetItem::Replace {
                target,
                value: value(),
                span,
            },
            It::Merge(..) => GqlSetItem::Merge {
                target,
                value: value(),
                span,
            },
            It::Labels(_, labels) => GqlSetItem::Labels {
                target,
                labels: labels.iter().map(|l| CompactString::from(*l)).collect(),
                span,
            },
            It::Key(..) => panic!("not a SET item"),
        });
    }
    (query, out)
}

fn set<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    items: &[It],
) -> GraphTraversalSource<'g, S> {
    set_with(start, items, Some(ElementKind::Vertex), &[])
}

/// `start.gql_remove(items)`, targets as in [`set_with`].
fn remove_with<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    items: &[It],
    element: Option<ElementKind>,
    labels: &[&str],
) -> GraphTraversalSource<'g, S> {
    let (query, _, marker) = build(items);
    let mut from = marker;
    let mut out = Vec::new();
    for item in items {
        let span = span_of(&query, &item.text(), from);
        from = span.end;
        let target = target(&query, *item, span, element, labels);
        out.push(match *item {
            It::Key(_, key) => GqlRemoveItem::Property {
                target,
                key: key.into(),
                span,
            },
            It::Labels(_, labels) => GqlRemoveItem::Labels {
                target,
                labels: labels.iter().map(|l| CompactString::from(*l)).collect(),
                span,
            },
            other => panic!("{:?} is not a REMOVE item", other.text()),
        });
    }
    start.gql_remove(query, out)
}

fn remove<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    items: &[It],
) -> GraphTraversalSource<'g, S> {
    remove_with(start, items, Some(ElementKind::Vertex), &[])
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

fn help(error: &TraverserError) -> String {
    error.help().unwrap_or_default()
}

fn named(graph: &TraversalGraph, name: &str) -> VertexHandle {
    graph
        .vertices()
        .find(|v| {
            graph
                .vertex_properties(*v)
                .and_then(|p| p.get("name"))
                .and_then(ElementProperty::as_string)
                == Some(name)
        })
        .unwrap_or_else(|| panic!("no vertex {name}"))
}

/// The properties of the vertex named `name`, as `(key, value)` in stored order.
fn props(graph: &TraversalGraph, name: &str) -> Vec<(String, ElementProperty)> {
    let v = named(graph, name);
    graph
        .vertex_properties(v)
        .map(|p| p.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
        .unwrap_or_default()
}

fn kv(pairs: &[(&str, ElementProperty)]) -> Vec<(String, ElementProperty)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn marko<'g>(graph: &'g mut TraversalGraph) -> GraphTraversalSource<'g, &'g mut TraversalGraph> {
    graph.traversal_mut().v(None).has("name", "marko")
}

/// A canonical dump of the whole graph: every vertex's labels and properties, every edge's
/// label and properties.
fn dump(graph: &TraversalGraph) -> String {
    let mut out = Vec::new();
    for v in graph.vertices() {
        out.push(format!(
            "v {:?} {:?}",
            graph.vertex_labels(v),
            graph.vertex_properties(v)
        ));
    }
    for e in graph.edges() {
        out.push(format!(
            "e {:?} {:?}",
            graph.edge_label(e),
            graph.edge_properties(e)
        ));
    }
    out.sort();
    out.join("\n")
}

#[test]
fn property_items_set_and_a_null_value_removes() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // Left to right, each item sees the earlier ones (W22); a repeated key: the last wins.
    let out = set(
        marko(&mut graph),
        &[
            It::Prop("n", "a", "1"),
            It::Prop("n", "b", "n.a + 1"),
            It::Prop("n", "a", "10"),
            It::Prop("n", "doc", "{k: [1, 'x'], m: NULL}"),
        ],
    )
    .values("name")
    .to_list()
    .unwrap();
    // The row is emitted unchanged.
    assert_eq!(out, [ElementProperty::from("marko")]);
    let stored = props(&graph, "marko");
    assert!(stored.contains(&("a".into(), int(10))), "{stored:?}");
    assert!(stored.contains(&("b".into(), int(2))), "{stored:?}");
    // W21: maps and mixed lists are stored (NULL inside a map is part of the value).
    assert!(stored.iter().any(|(k, v)| k == "doc"
        && matches!(v, ElementProperty::Object(m) if m.get("m") == Some(&ElementProperty::Null))));
    // A NULL value removes the key (never a stored null); a missing key is a no-op.
    set(
        marko(&mut graph),
        &[
            It::Prop("n", "age", "NULL"),
            It::Prop("n", "nope", "NULL"),
            It::Prop("n", "b", "n.missing"),
        ],
    )
    .iterate()
    .unwrap();
    let keys: Vec<String> = props(&graph, "marko").into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, ["name", "a", "doc"]);
    // Edges likewise.
    set_with(
        graph.traversal_mut().e(None).has_label("knows"),
        &[
            It::Prop("n", "weight", "NULL"),
            It::Prop("n", "since", "2020"),
        ],
        Some(ElementKind::Edge),
        &["knows"],
    )
    .iterate()
    .unwrap();
    let knows: Vec<PropertyMap> = graph
        .edges()
        .filter(|e| graph.edge_label(*e) == Some("knows"))
        .map(|e| graph.edge_properties(e).cloned().unwrap_or_default())
        .collect();
    assert_eq!(knows.len(), 2);
    for p in knows {
        let keys: Vec<&str> = p.keys().map(CompactString::as_str).collect();
        assert_eq!(keys, ["since"]);
    }
    // A node, an edge or a path is no property value (22G03).
    let error = root(set(marko(&mut graph), &[It::Prop("n", "v", "[1, n]")]).iterate());
    let (kind, message, status) = gql_status(&error);
    assert_eq!((kind, status.as_str()), (GqlErrorKind::Data, "22G03"));
    assert!(message.contains("property `v` is a node"), "{message}");
}

#[test]
fn replace_and_merge() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // `=` replaces every property; NULL entries are absent.
    set(
        marko(&mut graph),
        &[It::Replace("n", "{name: 'marko', k: 1, gone: NULL}")],
    )
    .iterate()
    .unwrap();
    assert_eq!(
        props(&graph, "marko"),
        kv(&[("name", "marko".into()), ("k", int(1))])
    );
    // `+=` merges; a NULL entry removes its key; a parameter map.
    set(
        marko(&mut graph),
        &[It::Merge("n", "{age: 30, k: NULL}"), It::Merge("n", "$p")],
    )
    .iterate()
    .unwrap();
    assert_eq!(
        props(&graph, "marko"),
        kv(&[("name", "marko".into()), ("age", int(30)), ("k", int(1))])
    );
    // `+= {}` changes nothing; `= {}` removes everything.
    let before = dump(&graph);
    set(marko(&mut graph), &[It::Merge("n", "{}")])
        .iterate()
        .unwrap();
    assert_eq!(dump(&graph), before);
    // An element's properties: copied (a snapshot taken before the first write); `SET n = n`
    // changes nothing.
    let vadas = graph.traversal_mut().v(None).has("name", "vadas").as_("a");
    set(
        vadas.v(None).has("name", "josh"),
        &[It::Replace("n", "a"), It::Prop("n", "copied", "true")],
    )
    .iterate()
    .unwrap();
    assert_eq!(
        props_of_copy(&graph),
        [
            ("name".into(), "vadas".into()),
            ("age".into(), int(27)),
            ("copied".into(), true.into())
        ]
    );
    let before = dump(&graph);
    set(marko(&mut graph), &[It::Replace("n", "n")])
        .iterate()
        .unwrap();
    assert_eq!(dump(&graph), before);
    set(marko(&mut graph), &[It::Merge("n", "n")])
        .iterate()
        .unwrap();
    assert_eq!(dump(&graph), before);
    // `properties(m)` is a plain map.
    set(
        graph
            .traversal_mut()
            .v(None)
            .has("name", "lop")
            .as_("a")
            .v(None)
            .has("name", "ripple"),
        &[It::Merge("n", "properties(a)")],
    )
    .iterate()
    .unwrap();
    assert_eq!(props(&graph, "lop").len(), 2);
    assert!(props(&graph, "lop").contains(&("lang".to_string(), "java".into())));
    let marko_v = named(&graph, "marko");
    set(marko(&mut graph), &[It::Replace("n", "{}")])
        .iterate()
        .unwrap();
    assert!(
        graph
            .vertex_properties(marko_v)
            .is_none_or(PropertyMap::is_empty)
    );
}

/// The properties of the vertex the copy test wrote to (josh, now a copy of vadas).
fn props_of_copy(graph: &TraversalGraph) -> Vec<(CompactString, ElementProperty)> {
    let copy = graph
        .vertices()
        .find(|v| {
            graph
                .vertex_properties(*v)
                .is_some_and(|p| p.contains_key("copied"))
        })
        .unwrap();
    graph
        .vertex_properties(copy)
        .map(|p| p.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

#[test]
fn a_failing_item_leaves_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = dump(&graph);
    // A bad entry fails the item before its first write (the `property(map)` rule), and the
    // failed traversal leaves nothing of the earlier items either.
    for items in [
        &[It::Replace("n", "{name: 'm2', bad: [n]}")][..],
        &[It::Prop("n", "a", "1"), It::Merge("n", "{k: 1, bad: n}")][..],
        &[
            It::Prop("n", "a", "1"),
            It::Labels("n", &["L"]),
            It::Prop("n", "b", "1 / 0"),
        ][..],
    ] {
        let error = root(set(marko(&mut graph), items).iterate());
        assert!(
            matches!(error.root_cause(), TraverserError::Gql { .. }),
            "{error}"
        );
        assert_eq!(dump(&graph), before);
    }
    // D4: a NULL right side of `=` / `+=` is 22004; a value that is no map 22G03.
    for (item, status, text) in [
        (It::Replace("n", "NULL"), "22004", "SET n = {}"),
        (It::Merge("n", "n.missing"), "22004", "coalesce($p, {})"),
        (It::Replace("n", "1"), "22G03", "SET n.k = <value>"),
        (It::Merge("n", "[1]"), "22G03", "SET n += {k: 1}"),
    ] {
        let error = root(set(marko(&mut graph), &[item]).iterate());
        let (kind, message, code) = gql_status(&error);
        assert_eq!(
            (kind, code.as_str()),
            (GqlErrorKind::Data, status),
            "{message}"
        );
        assert!(help(&error).contains(text), "{}", help(&error));
    }
    assert_eq!(dump(&graph), before);
}

#[test]
fn labels_are_added_and_removed() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let marko_v = named(&graph, "marko");
    // Idempotent, in order.
    for _ in 0..2 {
        set(
            marko(&mut graph),
            &[It::Labels("n", &["A", "B"]), It::Labels("n", &["A"])],
        )
        .iterate()
        .unwrap();
        assert_eq!(graph.vertex_labels(marko_v), ["person", "A", "B"]);
    }
    // REMOVE: a missing label is a no-op; the last label may go.
    remove(marko(&mut graph), &[It::Labels("n", &["A", "nope"])])
        .iterate()
        .unwrap();
    assert_eq!(graph.vertex_labels(marko_v), ["person", "B"]);
    remove(marko(&mut graph), &[It::Labels("n", &["person", "B"])])
        .iterate()
        .unwrap();
    assert!(graph.vertex_labels(marko_v).is_empty());
    // An edge has one label: 22G03 naming set_label(), for SET and REMOVE.
    for result in [
        set_with(
            graph.traversal_mut().e(None).has_label("knows").limit(1),
            &[It::Labels("n", &["x"])],
            None,
            &[],
        )
        .iterate(),
        remove_with(
            graph.traversal_mut().e(None).has_label("knows").limit(1),
            &[It::Labels("n", &["knows"])],
            None,
            &[],
        )
        .iterate(),
    ] {
        let error = root(result);
        let (kind, message, status) = gql_status(&error);
        assert_eq!((kind, status.as_str()), (GqlErrorKind::Data, "22G03"));
        assert!(message.ends_with("but `n` holds an edge"), "{message}");
        assert!(help(&error).contains("set_label("), "{}", help(&error));
    }
}

#[test]
fn remove_properties() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    remove(
        marko(&mut graph),
        &[It::Key("n", "age"), It::Key("n", "nope")],
    )
    .iterate()
    .unwrap();
    assert_eq!(props(&graph, "marko"), kv(&[("name", "marko".into())]));
    remove_with(
        graph.traversal_mut().e(None).has_label("created"),
        &[It::Key("n", "weight")],
        Some(ElementKind::Edge),
        &["created"],
    )
    .iterate()
    .unwrap();
    assert!(
        graph
            .edges()
            .filter(|e| graph.edge_label(*e) == Some("created"))
            .all(|e| graph.edge_properties(e).is_none_or(PropertyMap::is_empty))
    );
}

#[test]
fn a_null_target_is_a_no_op_whatever_the_item() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = dump(&graph);
    // `x` is never labelled: NULL (an OPTIONAL MATCH that did not match). The right side is
    // not evaluated (`1 / 0` would fail).
    let out = set(
        marko(&mut graph),
        &[
            It::Prop("x", "k", "1 / 0"),
            It::Replace("x", "NULL"),
            It::Merge("x", "1"),
            It::Labels("x", &["A"]),
        ],
    )
    .count()
    .next()
    .unwrap();
    assert_eq!(out, Some(int(1)));
    remove(
        marko(&mut graph),
        &[It::Key("x", "name"), It::Labels("x", &["person"])],
    )
    .iterate()
    .unwrap();
    // A stored null in a row column is NULL too.
    set(
        graph.traversal_mut().inject(vec![ElementProperty::Null]),
        &[It::Prop("n", "k", "1")],
    )
    .iterate()
    .unwrap();
    assert_eq!(dump(&graph), before);
}

#[test]
fn a_removed_or_non_element_target_is_an_error() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // Removed earlier in the query: the storage's error, as for `property()` (W27).
    fn dropped(graph: &mut TraversalGraph) -> GraphTraversalSource<'_, &mut TraversalGraph> {
        graph
            .traversal_mut()
            .v(None)
            .has("name", "marko")
            .as_("a")
            .side_effect_t(__::drop())
    }
    let set_result = set(dropped(&mut graph), &[It::Prop("a", "k", "1")]).iterate();
    let gremlin = dropped(&mut graph).select("a").property("k", 1).iterate();
    let remove_result = remove(dropped(&mut graph), &[It::Labels("a", &["person"])]).iterate();
    for result in [set_result, gremlin, remove_result] {
        let error = root(result);
        assert!(
            matches!(
                error.root_cause(),
                TraverserError::Graph(GraphError::ElementRemoved { .. })
            ),
            "{error}"
        );
    }
    // The properties of a removed element as the right side likewise.
    let error = root(
        set(
            dropped(&mut graph).v(None).has("name", "josh"),
            &[It::Merge("n", "a")],
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
    // A value that is no element: 22G03 naming the variable.
    for result in [
        set(
            graph.traversal_mut().inject(vec![1]),
            &[It::Prop("n", "k", "1")],
        )
        .iterate(),
        remove(
            graph.traversal_mut().inject(vec!["s"]),
            &[It::Key("n", "k")],
        )
        .iterate(),
    ] {
        let error = root(result);
        let (kind, message, status) = gql_status(&error);
        assert_eq!((kind, status.as_str()), (GqlErrorKind::Data, "22G03"));
        assert!(
            message.contains("but `n` holds a value of type"),
            "{message}"
        );
    }
    assert_eq!(graph.vertices().count(), 6);
}

const SCHEMA: &str = r#"{
  "mode": "open",
  "vertices": {
    "item": {"schema": {"type": "object", "required": ["name"], "properties": {
      "name": {"type": "string"},
      "n": {"type": "integer"},
      "at": {"type": "string", "format": "date-time"}
    }}}
  }
}"#;

#[test]
fn the_schema_coerces_and_enforces_through_the_storage_write_path() {
    let mut graph = TraversalGraph::new();
    let schema = GraphSchema::from_json(SCHEMA).unwrap();
    graph.traversal_mut().set_schema(schema).unwrap();
    graph
        .traversal_mut()
        .add_v("item")
        .property("name", "a")
        .iterate()
        .unwrap();
    fn item(graph: &mut TraversalGraph) -> GraphTraversalSource<'_, &mut TraversalGraph> {
        graph.traversal_mut().v(None).has_label("item")
    }
    // Write-time coercion of a declared property.
    set(
        item(&mut graph),
        &[It::Prop("n", "at", "'2026-10-15T12:00:00Z'")],
    )
    .iterate()
    .unwrap();
    assert!(
        matches!(
            props(&graph, "a").iter().find(|(k, _)| k == "at"),
            Some((_, ElementProperty::Temporal(_)))
        ),
        "{:?}",
        props(&graph, "a")
    );
    let before = dump(&graph);
    // `required`: removing it by `= {}`, a NULL value or REMOVE is a schema violation; a type
    // violation likewise.
    for result in [
        set(item(&mut graph), &[It::Replace("n", "{}")]).iterate(),
        set(item(&mut graph), &[It::Replace("n", "{n: 1}")]).iterate(),
        set(item(&mut graph), &[It::Prop("n", "name", "NULL")]).iterate(),
        set(item(&mut graph), &[It::Merge("n", "{name: NULL}")]).iterate(),
        remove(item(&mut graph), &[It::Key("n", "name")]).iterate(),
        set(item(&mut graph), &[It::Prop("n", "n", "'one'")]).iterate(),
    ] {
        let error = root(result);
        assert!(
            matches!(
                error.root_cause(),
                TraverserError::Graph(GraphError::Schema(_))
            ),
            "{error}"
        );
        assert_eq!(dump(&graph), before);
    }
    // Replacing with a map that keeps the required key works.
    set(item(&mut graph), &[It::Replace("n", "{name: 'b', n: 2}")])
        .iterate()
        .unwrap();
    assert_eq!(
        props(&graph, "b"),
        kv(&[("name", "b".into()), ("n", int(2))])
    );
}

#[test]
fn every_unit_of_bulk_is_one_row() {
    use crate::traversal::TraversalStrategy;
    use std::collections::HashMap;
    // marko is reached three times (marko -> lop, vadas, josh -> in): `n.c = coalesce(n.c, 0)
    // + 1` runs three times, whatever the merge option (a mutating plan never merges, e10).
    for merge in [true, false] {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let out = set(
            graph
                .traversal_mut()
                .with("bulk.merge", merge)
                .v(None)
                .has("name", "marko")
                .out(None)
                .in_(None)
                .has("name", "marko"),
            &[It::Prop("n", "c", "coalesce(n.c, 0) + 1")],
        )
        .to_list()
        .unwrap();
        assert_eq!(out.len(), 3);
        assert!(props(&graph, "marko").contains(&("c".into(), int(3))));
    }
    // A traverser of bulk 3 is three rows of bulk 1 (W15).
    let mut graph = TraversalGraph::tinkerpop_modern();
    let marko_v = named(&graph, "marko");
    let (query, items) = set_items(&[It::Prop("n", "c", "coalesce(n.c, 0) + 1")], None, &[]);
    let mut steps = AnonymousTraversal::<TraversalGraph>::new()
        .gql_set(query, items, bindings(), Vec::new())
        .steps;
    let GraphStep::SideEffectMut(_, step) = &mut steps[0] else {
        panic!("not a SideEffectMut");
    };
    let mut context = TraverserContext::new(&mut graph, TraversalStrategy::none(), HashMap::new());
    let mut source: Traverser<TraversalGraph> =
        Traverser::from_value(TraverserValue::Vertex(marko_v));
    source.bulk = 3;
    let rows = step.process(&mut context, Some(source)).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.bulk == 1));
    // No input: nothing to change, nothing emitted.
    assert!(step.process(&mut context, None).unwrap().is_empty());
    // An untrusted bulk is checked before any row is copied.
    context.options.bulk_max_expansion = 2;
    let mut source: Traverser<TraversalGraph> =
        Traverser::from_value(TraverserValue::Vertex(marko_v));
    source.bulk = 3;
    assert!(step.process(&mut context, Some(source)).is_err());
    drop(context);
    assert!(props(&graph, "marko").contains(&("c".into(), int(3))));
}

/// An authorizer recording every request, allowing all but `Update` when `deny_update`.
struct Recorder {
    seen: Mutex<Vec<AccessRequestBuf>>,
    deny_update: bool,
}

impl Authorizer for Recorder {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(request.to_owned_request());
        }
        if self.deny_update && matches!(request.action, crate::graph::Action::Update) {
            return Err(Denied::new("no updates"));
        }
        Ok(())
    }
}

fn recorder(deny_update: bool) -> Arc<Recorder> {
    Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
        deny_update,
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
fn access_requests_equal_the_gremlin_twins() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    type Source<'g> = GraphTraversalSource<'g, &'g mut TraversalGraph>;
    type Twin = fn(Source<'_>) -> Source<'_>;
    // (GQL items, start from edges, target kind, edge labels, the Gremlin twin)
    type Case = (
        &'static [It],
        bool,
        Option<ElementKind>,
        &'static [&'static str],
        Twin,
    );
    let cases: Vec<Case> = vec![
        (
            &[It::Prop("n", "x", "1")],
            false,
            Some(ElementKind::Vertex),
            &[],
            |g| g.property("x", 1),
        ),
        (
            &[It::Prop("n", "x", "NULL")],
            false,
            Some(ElementKind::Vertex),
            &[],
            |g| g.remove_property("x"),
        ),
        (
            &[It::Merge("n", "{a: 1}")],
            false,
            Some(ElementKind::Vertex),
            &[],
            |g| g.property("a", 1),
        ),
        (
            &[It::Prop("n", "w", "1")],
            true,
            Some(ElementKind::Edge),
            &["knows"],
            |g| g.property("w", 1),
        ),
        // The kind unknown (a FOR item): `Data { None, None }`, as `property()` after a step
        // the plan does not state.
        (&[It::Prop("n", "x", "1")], false, None, &[], |g| {
            g.fold().unfold().property("x", 1)
        }),
        (
            &[It::Labels("n", &["A", "B"])],
            false,
            Some(ElementKind::Vertex),
            &[],
            |g| g.add_label("A").add_label("B"),
        ),
    ];
    for (items, edges, element, labels, twin) in cases {
        let gql = recorder(false);
        let start = graph.traversal_mut().with_authorizer(gql.clone());
        let start = if edges {
            start.e(None).has_label("knows")
        } else {
            start.v(None).has("name", "marko")
        };
        let start = if element.is_none() {
            start.fold().unfold()
        } else {
            start
        };
        set_with(start, items, element, labels).iterate().unwrap();
        let gremlin = recorder(false);
        let start = graph.traversal_mut().with_authorizer(gremlin.clone());
        let start = if edges {
            start.e(None).has_label("knows")
        } else {
            start.v(None).has("name", "marko")
        };
        twin(start).iterate().unwrap();
        let texts: Vec<String> = items.iter().map(|i| i.text()).collect();
        assert_eq!(seen(&gql), seen(&gremlin), "{texts:?}");
        assert!(
            seen(&gql).iter().any(|r| r.starts_with("Update")),
            "{texts:?}"
        );
    }
    // REMOVE asks what `remove_property()` / `drop_label()` ask.
    for (items, twin) in [
        (
            &[It::Key("n", "age")][..],
            (|g| g.remove_property("age")) as Twin,
        ),
        (&[It::Labels("n", &["A"])][..], |g| g.drop_label("A")),
    ] {
        let gql = recorder(false);
        remove(
            graph
                .traversal_mut()
                .with_authorizer(gql.clone())
                .v(None)
                .has("name", "marko"),
            items,
        )
        .iterate()
        .unwrap();
        let gremlin = recorder(false);
        twin(
            graph
                .traversal_mut()
                .with_authorizer(gremlin.clone())
                .v(None)
                .has("name", "marko"),
        )
        .iterate()
        .unwrap();
        assert_eq!(seen(&gql), seen(&gremlin));
    }
    // The requests come from the step's own target facts, never from the flow: a SET of an
    // edge variable on a vertex row names the edge.
    let steps = set_with(
        GraphTraversalSource::new(&graph),
        &[It::Prop("a", "w", "1"), It::Labels("n", &["L"])],
        Some(ElementKind::Edge),
        &["knows", "created"],
    )
    .take_steps();
    let vertex_flow = AccessFlow {
        element: Some(ElementKind::Vertex),
        labels: vec!["person".into()],
    };
    let mut requests = Vec::new();
    steps[0]
        .for_each_access_in(&vertex_flow, |request| {
            requests.push(request.to_string());
            Ok::<(), ()>(())
        })
        .unwrap();
    assert_eq!(
        requests,
        [
            r#"Update Data(edge "knows")"#,
            r#"Update Data(edge "created")"#,
            "Update Data(vertex)",
        ]
    );
    assert!(steps[0].mutates());
    // The row passes on; a label item forgets its labels, a property item keeps them.
    let after = steps[0].access_flow_after(vertex_flow.clone());
    assert_eq!(after.element, Some(ElementKind::Vertex));
    assert!(after.labels.is_empty());
    let steps = set(
        GraphTraversalSource::new(&graph),
        &[It::Prop("n", "w", "1")],
    )
    .take_steps();
    assert_eq!(steps[0].access_flow_after(vertex_flow.clone()), vertex_flow);
    let steps = remove(GraphTraversalSource::new(&graph), &[It::Key("n", "w")]).take_steps();
    assert!(steps[0].mutates());
}

#[test]
fn a_denied_update_changes_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = dump(&graph);
    for result in [
        set(
            graph
                .traversal_mut()
                .with_authorizer(recorder(true))
                .v(None)
                .has("name", "marko"),
            &[It::Prop("n", "x", "1")],
        )
        .iterate(),
        remove(
            graph
                .traversal_mut()
                .with_authorizer(recorder(true))
                .v(None)
                .has("name", "marko"),
            &[It::Labels("n", &["person"])],
        )
        .iterate(),
        set(
            graph
                .traversal_mut()
                .with_authorizer(Arc::new(AccessPolicy::read_only()))
                .v(None),
            &[It::Merge("n", "{}")],
        )
        .iterate(),
    ] {
        let error = root(result);
        assert!(
            matches!(error.root_cause(), TraverserError::PermissionDenied { .. }),
            "{error}"
        );
    }
    assert_eq!(dump(&graph), before);
}

#[test]
fn profile_text_and_path_requirements() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let start = graph
        .traversal_mut()
        .v(None)
        .has("name", "marko")
        .as_("a")
        .out(None);
    let profile = set(
        start,
        &[It::Prop("n", "of", "a.name"), It::Labels("n", &["A", "B"])],
    )
    .profile()
    .unwrap();
    let names: Vec<String> = profile
        .metrics()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    assert!(
        names.contains(&r#"gql:set("n.of = a.name, n:A:B")"#.to_string()),
        "{profile}"
    );
    // The value reads the label `a` (rule 13): the path keeps it up to the step.
    let text = profile.to_string();
    assert!(text.contains("[path: labels(a)]"), "{text}");
    // A target read through a label is recorded too.
    {
        let steps = remove(
            GraphTraversalSource::new(&graph),
            &[It::Key("a", "age"), It::Labels("n", &["A"])],
        )
        .take_steps();
        assert_eq!(steps[0].get_name(), r#"gql:remove("a.age, n:A")"#);
        let GraphStep::SideEffectMut(_, step) = &steps[0] else {
            panic!("not a SideEffectMut");
        };
        assert_eq!(
            step.path_requirement(),
            crate::traversal::optimizer::path_analysis::PathRequirement::labels([
                CompactString::from("a")
            ])
        );
    }
    // Under camel case the GQL text in the literal is not respelled.
    let camel = set(
        graph
            .traversal_mut()
            .with("render.spelling", "camel")
            .v(None)
            .has("name", "marko"),
        &[It::Prop("n", "k", "char_length('a')")],
    )
    .profile()
    .unwrap()
    .to_string();
    assert!(camel.contains("gql:set("), "{camel}");
    assert!(camel.contains("char_length('a')"), "{camel}");
}
