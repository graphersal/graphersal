//! The stage 2a internal step `gql:for` on hand-built pipelines (the lowering that builds it is
//! 2a.T4): items as the current object, record mode with an ordinal, `NULL` and non-list lists
//! (design E1), bulk, path, sack, profile text and errors.

use std::sync::Arc;

use super::*;
use crate::gql::ast::{OrdinalKind, Query};
use crate::gql::eval::compile;
use crate::gql::parser::parse;
use crate::traversal::step::flatmap_steps::step_gql_for::{GqlOrdinal, StepGqlFor};
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::{GraphTraversalSource, Operator, TraversalGraph};

fn query(text: &str) -> (Arc<str>, Query) {
    match parse(text) {
        Ok(parsed) => (Arc::from(text), parsed),
        Err(error) => panic!("{text:?} does not parse: {error}"),
    }
}

/// The list expression of `MATCH (n) RETURN <list>` compiled with `n` the current element and
/// `a` the object labelled `a`; `$ids` is `[1, 2, 2]`, `$nothing` is `null`.
fn list<S: GraphStorage>(list: &str) -> (Arc<str>, RowExpr<S>, Arc<[Binding]>) {
    let (query, parsed) = query(&format!("MATCH (n) RETURN {list}"));
    let names = [CompactString::from("n"), CompactString::from("a")];
    let bindings: Arc<[Binding]> = Arc::from([Binding::Current, Binding::Label("a".into())]);
    let scope = BindingScope {
        names: &names,
        parameter: |name: &str| match name {
            "ids" => Some(ElementProperty::Array(vec![int(1), int(2), int(2)])),
            "nothing" => Some(ElementProperty::Null),
            _ => None,
        },
    };
    let expr = match compile(&parsed.ret.items[0].expr, &scope) {
        Ok(expr) => expr,
        Err(error) => panic!("{list:?} does not compile: {error:?}"),
    };
    (query, expr, bindings)
}

/// `start.gql_for(x IN <list>)` with an optional ordinal.
fn for_each<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    text: &str,
    ordinal: Option<(&str, OrdinalKind)>,
) -> GraphTraversalSource<'g, S> {
    let (query, expr, bindings) = list(text);
    start.gql_for(
        query,
        "x".into(),
        expr,
        bindings,
        ordinal.map(|(name, kind)| GqlOrdinal {
            name: name.into(),
            kind,
        }),
    )
}

fn int(value: i64) -> ElementProperty {
    ElementProperty::from(value)
}

fn unit(graph: &TraversalGraph) -> GraphTraversalSource<'_, &TraversalGraph> {
    graph.traversal().inject(vec![0])
}

fn items(start: GraphTraversalSource<'_, &TraversalGraph>, text: &str) -> Vec<ElementProperty> {
    for_each(start, text, None).to_list().unwrap()
}

#[test]
fn items_are_the_current_object_in_list_order() {
    let graph = TraversalGraph::tinkerpop_modern();
    // Duplicates and NULL items are rows of their own.
    assert_eq!(
        items(unit(&graph), "[1, 2, 2, NULL, 'a', [3, 4]]"),
        [
            int(1),
            int(2),
            int(2),
            ElementProperty::Null,
            "a".into(),
            ElementProperty::Array(vec![int(3), int(4)]),
        ]
    );
    assert_eq!(items(unit(&graph), "$ids"), [int(1), int(2), int(2)]);
    assert!(items(unit(&graph), "[]").is_empty());
}

#[test]
fn a_null_list_gives_no_row_and_a_non_list_one_row() {
    let graph = TraversalGraph::tinkerpop_modern();
    // E1: NULL -> no row; a value that is not a list -> one row of that value.
    assert!(items(unit(&graph), "NULL").is_empty());
    assert!(items(unit(&graph), "$nothing").is_empty());
    assert_eq!(items(unit(&graph), "5"), [int(5)]);
    assert_eq!(items(unit(&graph), "'abc'"), ["abc".into()]);
    let map = items(unit(&graph), "{k: 1}");
    assert_eq!(map.len(), 1, "{map:?}");
    assert!(matches!(&map[0], ElementProperty::Object(_)), "{map:?}");
    // Per row: a person without `lang` gives none, a software one row.
    let langs = items(graph.traversal().v(None), "n.lang");
    assert_eq!(langs, ["java".into(), "java".into()]);
}

#[test]
fn element_items_stay_live_handles() {
    let graph = TraversalGraph::tinkerpop_modern();
    // Every person twice as a list item: the pattern walks on from the handle.
    let mut made: Vec<String> = for_each(
        graph.traversal().v(None).has_label("person"),
        "[n, n]",
        None,
    )
    .out("created")
    .values("name")
    .to_list()
    .unwrap()
    .iter()
    .map(|value| value.as_string().unwrap_or("?").to_string())
    .collect();
    made.sort();
    assert_eq!(
        made,
        ["lop", "lop", "lop", "lop", "lop", "lop", "ripple", "ripple"]
    );
}

#[test]
fn ordinality_and_offset_emit_records() {
    let graph = TraversalGraph::tinkerpop_modern();
    let rows = |kind| {
        for_each(unit(&graph), "['a', NULL, 'a']", Some(("i", kind)))
            .to_list()
            .unwrap()
            .into_iter()
            .map(|row| match row {
                ElementProperty::Object(map) => map
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.clone()))
                    .collect::<Vec<_>>(),
                other => panic!("not a record: {other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let record = |x: ElementProperty, i: i64| vec![("x".to_string(), x), ("i".to_string(), int(i))];
    assert_eq!(
        rows(OrdinalKind::Ordinality),
        [
            record("a".into(), 1),
            record(ElementProperty::Null, 2),
            record("a".into(), 3),
        ]
    );
    assert_eq!(
        rows(OrdinalKind::Offset),
        [
            record("a".into(), 0),
            record(ElementProperty::Null, 1),
            record("a".into(), 2),
        ]
    );
    // A non-list value is the first position; NULL still gives no row.
    assert_eq!(
        rows_of(for_each(
            unit(&graph),
            "7",
            Some(("i", OrdinalKind::Ordinality))
        )),
        1
    );
    assert_eq!(
        rows_of(for_each(
            unit(&graph),
            "NULL",
            Some(("i", OrdinalKind::Offset))
        )),
        0
    );

    // The record is a row a later step reads by column: `Binding::Column` while it is the
    // current object, `Binding::RowColumn` through its label after leaving it.
    let column = |binding: [Binding; 2]| {
        let (query, parsed) = query("MATCH (n) RETURN x + i AS s");
        let names = [CompactString::from("x"), CompactString::from("i")];
        let scope = BindingScope {
            names: &names,
            parameter: |_: &str| None,
        };
        let columns = vec![GqlColumn {
            name: "s".into(),
            expr: compile(&parsed.ret.items[0].expr, &scope).unwrap(),
        }];
        let bindings: Arc<[Binding]> = Arc::from(binding);
        (query, columns, bindings)
    };
    let sums =
        |start: GraphTraversalSource<'_, &TraversalGraph>| start.select("s").to_list().unwrap();
    let (q, columns, bindings) = column([Binding::Column(0), Binding::Column(1)]);
    assert_eq!(
        sums(
            for_each(unit(&graph), "[10, 20]", Some(("i", OrdinalKind::Offset)))
                .gql_project(q, columns, bindings)
        ),
        [int(10), int(21)]
    );
    let row_column = |column| Binding::RowColumn {
        label: "#f1".into(),
        column,
    };
    let (q, columns, bindings) = column([row_column(0), row_column(1)]);
    assert_eq!(
        sums(
            for_each(
                unit(&graph),
                "[10, 20]",
                Some(("i", OrdinalKind::Ordinality))
            )
            .as_("#f1")
            .constant("elsewhere")
            .gql_project(q, columns, bindings)
        ),
        [int(11), int(22)]
    );
}

fn rows_of(t: GraphTraversalSource<'_, &TraversalGraph>) -> usize {
    t.to_list().unwrap().len()
}

#[test]
fn bulk_is_kept_per_item() {
    let graph = TraversalGraph::tinkerpop_modern();
    // lop arrives three times (one traverser of bulk 3 after the barrier), ripple once; two
    // items each: 8 rows.
    let build = |merge: bool| {
        for_each(
            graph
                .traversal()
                .with("bulk.merge", merge)
                .v(None)
                .out(None)
                .has_label("software")
                .barrier(),
            "[n.name, n.name]",
            None,
        )
    };
    let mut merged = build(true).to_list().unwrap();
    let mut plain = build(false).to_list().unwrap();
    merged.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    plain.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(merged, plain);
    assert_eq!(merged.len(), 8, "{merged:?}");
    assert_eq!(
        merged.iter().filter(|v| **v == "lop".into()).count(),
        6,
        "{merged:?}"
    );
    // The merged run really emitted one traverser per item of each merged input.
    let profile = build(true).profile().unwrap();
    let metrics = profile
        .metrics()
        .iter()
        .find(|m| m.name().starts_with("gql:for"))
        .unwrap_or_else(|| panic!("no gql:for in {profile}"));
    assert_eq!(metrics.counts().traverser_count(), 4, "{profile}");
    assert_eq!(metrics.name(), r#"gql:for("x IN [n.name, n.name]")"#);
}

#[test]
fn path_and_sack_are_kept() {
    let graph = TraversalGraph::tinkerpop_modern();
    // The list reads `a` (a path label) besides the current element.
    let build = || {
        for_each(
            graph
                .traversal()
                .v(None)
                .has_label("person")
                .as_("a")
                .as_("unused")
                .out("created"),
            "[a.name, n.name]",
            None,
        )
    };
    let mut pairs: Vec<ElementProperty> = build().to_list().unwrap();
    pairs.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(pairs.len(), 8, "{pairs:?}");
    assert_eq!(
        pairs.iter().filter(|v| **v == "josh".into()).count(),
        2,
        "{pairs:?}"
    );
    let profile = build().profile().unwrap().to_string();
    assert!(profile.contains("[path: labels(a)]"), "{profile}");
    assert!(!profile.contains("unused)]"), "{profile}");
    assert!(!profile.contains("[path: full]"), "{profile}");
    // The item is a new position on the row's path: person, software, item.
    for path in build().path().to_list().unwrap() {
        let ElementProperty::Array(objects) = path else {
            panic!("not a path: {path:?}");
        };
        assert_eq!(objects.len(), 3, "{objects:?}");
    }
    // The sack of each row reaches its items.
    let mut sacks: Vec<ElementProperty> = for_each(
        graph
            .traversal()
            .with_sack(0)
            .v(None)
            .has_label("person")
            .sack_with(Operator::Assign)
            .by("age"),
        "[1, 2]",
        None,
    )
    .sack()
    .to_list()
    .unwrap();
    sacks.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        sacks,
        [
            int(27),
            int(27),
            int(29),
            int(29),
            int(32),
            int(32),
            int(35),
            int(35)
        ]
    );
}

#[test]
fn profile_text_and_errors() {
    let graph = TraversalGraph::tinkerpop_modern();
    let name = |ordinal| {
        let profile = for_each(unit(&graph), "[1, 2]", ordinal).profile().unwrap();
        profile
            .metrics()
            .iter()
            .find(|m| m.name().starts_with("gql:for"))
            .map(|m| m.name().to_string())
            .unwrap_or_default()
    };
    assert_eq!(name(None), r#"gql:for("x IN [1, 2]")"#);
    assert_eq!(
        name(Some(("i", OrdinalKind::Ordinality))),
        r#"gql:for("x IN [1, 2] WITH ORDINALITY i")"#
    );
    assert_eq!(
        name(Some(("pos", OrdinalKind::Offset))),
        r#"gql:for("x IN [1, 2] WITH OFFSET pos")"#
    );
    // An evaluation error fails the traversal with the evaluator's GQLSTATUS.
    let error = for_each(unit(&graph), "[1 / 0]", None)
        .to_list()
        .unwrap_err();
    match error.root_cause() {
        TraverserError::Gql { gqlstatus, .. } => assert_eq!(gqlstatus, "22012"),
        other => panic!("not a GQL error: {other}"),
    }
    // A cancelled run stops between items.
    let token = crate::exec::CancelToken::new();
    token.cancel();
    let error = token
        .scope(|| for_each(unit(&graph), "$ids", None).to_list())
        .unwrap_err();
    assert!(
        matches!(error.root_cause(), TraverserError::Cancelled { .. }),
        "{error}"
    );
}

#[test]
fn reads_no_permission_and_writes_nothing() {
    let (query, expr, bindings) = list::<&TraversalGraph>("[n]");
    let step = crate::GraphStep::FlatMap(
        "gql:for",
        Box::new(StepGqlFor::new(query, "x".into(), expr, bindings, None)),
    );
    let mut requests = 0;
    step.for_each_access(|_| {
        requests += 1;
        Ok::<(), ()>(())
    })
    .unwrap();
    assert_eq!(requests, 0);
    assert!(!step.mutates());
}
