//! The stage 2c internal step `gql:value` and the collecting host of a pattern comprehension on
//! hand-built pipelines (the lowering that builds them is 2c.T3): values as the current object,
//! the list in child order, bulk of the child's results and of the hosting row, `NULL` (absent)
//! imports, the limits checked before the list is built, the profile text.

use std::sync::Arc;

use super::*;
use crate::gql::ast::Query;
use crate::gql::eval::compile;
use crate::gql::lexer::Span;
use crate::gql::parser::parse;
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::{__, AnonymousTraversal, GraphTraversalSource, TraversalGraph};

fn query(text: &str) -> (Arc<str>, Query) {
    match parse(text) {
        Ok(parsed) => (Arc::from(text), parsed),
        Err(error) => panic!("{text:?} does not parse: {error}"),
    }
}

/// A scope with `n` the only variable whose first subquery is hosted as index 0.
struct HostScope<'a> {
    names: &'a [CompactString],
}

impl Scope for HostScope<'_> {
    fn variable(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.as_str() == name)
    }

    fn parameter(&self, _name: &str) -> Option<ElementProperty> {
        None
    }

    fn subquery(&self, _span: Span) -> Option<Result<usize, EvalError>> {
        Some(Ok(0))
    }
}

/// The `RETURN` expression of `MATCH (n) RETURN <expr>`, `n` the current object; a subquery in
/// it is hosted subquery 0.
fn expr<S: GraphStorage>(text: &str) -> (Arc<str>, RowExpr<S>) {
    let (query, parsed) = query(&format!("MATCH (n) RETURN {text}"));
    let names = [CompactString::from("n")];
    match compile(&parsed.ret.items[0].expr, &HostScope { names: &names }) {
        Ok(expr) => (query, expr),
        Err(error) => panic!("{text:?} does not compile: {error:?}"),
    }
}

fn current() -> Arc<[Binding]> {
    Arc::from([Binding::Current])
}

/// `child.gql_value(<value>)` with `n` the child's current object.
fn valued<'g, S: GraphStorage + 'g>(
    child: AnonymousTraversal<'g, S>,
    value: &str,
) -> AnonymousTraversal<'g, S> {
    let (query, expr) = expr(value);
    child.gql_value_hosted(query, expr, current(), Vec::new())
}

/// `start.gql_project(l: <list>)` hosting `child` as the comprehension the column holds.
fn collect<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    child: AnonymousTraversal<'g, S>,
) -> GraphTraversalSource<'g, S> {
    let (query, list) = expr("[(n)-->(m) | m.name]");
    start.gql_project_hosted(
        query,
        vec![GqlColumn {
            name: "l".into(),
            expr: list,
        }],
        current(),
        vec![child],
    )
}

/// The list column of every row.
fn lists(t: GraphTraversalSource<'_, &TraversalGraph>) -> Vec<Vec<String>> {
    t.to_list()
        .unwrap()
        .into_iter()
        .map(|row| match row {
            ElementProperty::Object(map) => match map.get("l") {
                Some(ElementProperty::Array(items)) => items
                    .iter()
                    .map(|item| item.as_string().unwrap_or("?").to_string())
                    .collect(),
                other => panic!("no list in {other:?}"),
            },
            other => panic!("not a row: {other:?}"),
        })
        .collect()
}

fn strings(values: Vec<ElementProperty>) -> Vec<String> {
    values
        .iter()
        .map(|value| value.as_string().unwrap_or("?").to_string())
        .collect()
}

#[test]
fn value_turns_each_traverser_into_its_value() {
    let graph = TraversalGraph::tinkerpop_modern();
    let (query, name) = expr("n.name");
    let names = graph
        .traversal()
        .v(None)
        .has_label("person")
        .gql_value_hosted(query, name, current(), Vec::new())
        .to_list()
        .unwrap();
    let twin = graph
        .traversal()
        .v(None)
        .has_label("person")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(names, twin);
    // A variable stays the live element handle: Gremlin walks on from it.
    let (query, n) = expr::<&TraversalGraph>("n");
    let created = graph
        .traversal()
        .v(None)
        .has_label("person")
        .gql_value_hosted(query, n, current(), Vec::new())
        .out("created")
        .values("name")
        .to_list()
        .unwrap();
    assert_eq!(created.len(), 4, "{created:?}");
    // Bulk passes through (a merged lop of bulk 3 stays three values).
    let (query, name) = expr("n.name");
    let lops = graph
        .traversal()
        .v(None)
        .out(None)
        .has_label("software")
        .barrier()
        .gql_value_hosted(query, name, current(), Vec::new())
        .to_list()
        .unwrap();
    assert_eq!(
        strings(lops).iter().filter(|n| *n == "lop").count(),
        3,
        "bulk lost"
    );
}

#[test]
fn a_collecting_host_lists_the_child_results_in_child_order() {
    let graph = TraversalGraph::tinkerpop_modern();
    let got = lists(collect(
        graph.traversal().v(None).has_label("person"),
        valued(__::out(None), "n.name"),
    ));
    let twin: Vec<Vec<String>> = graph
        .traversal()
        .v(None)
        .has_label("person")
        .project(vec!["l"])
        .by(__::out(None).values("name").fold())
        .to_list()
        .unwrap()
        .into_iter()
        .map(|row| match row {
            ElementProperty::Object(map) => match map.get("l") {
                Some(ElementProperty::Array(items)) => strings(items.clone()),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(got, twin);
    assert!(got.iter().any(|l| l.is_empty()), "{got:?}");
}

#[test]
fn bulk_of_the_child_is_expanded_and_the_row_bulk_kept() {
    let graph = TraversalGraph::tinkerpop_modern();
    // From lop: in() reaches marko, josh, peter; out() from them reaches lop three times, which
    // the barrier merges into one traverser of bulk 3.
    let child = || valued(__::in_(None).out(None).barrier(), "n.name");
    let run = |merge: bool| {
        let mut rows = lists(collect(
            graph
                .traversal()
                .with("bulk.merge", merge)
                .v(None)
                .has("name", "lop"),
            child(),
        ));
        for row in &mut rows {
            row.sort();
        }
        rows
    };
    let merged = run(true);
    assert_eq!(merged, run(false));
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged[0].iter().filter(|n| *n == "lop").count(),
        3,
        "{merged:?}"
    );
    // A hosting row of bulk 3 (lop merged) gets one list per unit, not three times as long.
    let rows = lists(collect(
        graph
            .traversal()
            .v(None)
            .out(None)
            .has("name", "lop")
            .barrier(),
        valued(__::in_(None), "n.name"),
    ));
    assert_eq!(rows.len(), 3, "{rows:?}");
    for row in rows {
        assert_eq!(row.len(), 3, "{row:?}");
    }
}

#[test]
fn an_absent_import_matches_nothing() {
    let graph = TraversalGraph::tinkerpop_modern();
    // The child moves to a label no step set (an import that is NULL): the list is empty, never
    // NULL.
    let got = lists(collect(
        graph.traversal().v(None).has_label("software"),
        valued(__::select("missing").out(None), "n.name"),
    ));
    assert_eq!(got, vec![Vec::<String>::new(); 2]);
}

#[test]
fn the_list_is_checked_before_it_is_built() {
    let graph = TraversalGraph::tinkerpop_modern();
    // `bulk.max_expansion` for a child result of bulk 3.
    let error = collect(
        graph
            .traversal()
            .with("bulk.max_expansion", 2)
            .v(None)
            .has("name", "lop"),
        valued(__::in_(None).out(None).barrier(), "n.name"),
    )
    .to_list()
    .unwrap_err();
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::BulkExpansionLimit { limit: 2, .. }
        ),
        "{error}"
    );
    // `traversal.max_materialized_bytes` without any bulk.
    let error = collect(
        graph
            .traversal()
            .with("traversal.max_materialized_bytes", 64)
            .v(None)
            .has("name", "marko"),
        valued(__::out(None), "n.name"),
    )
    .to_list()
    .unwrap_err();
    assert!(
        matches!(
            error.root_cause(),
            TraverserError::ResourceLimitExceeded { limit, .. }
                if limit == "traversal.max_materialized_bytes"
        ),
        "{error}"
    );
}

#[test]
fn the_profile_shows_the_child_ending_in_gql_value() {
    let graph = TraversalGraph::tinkerpop_modern();
    let profile = collect(
        graph.traversal().v(None).has_label("person"),
        valued(__::out(None), "n.name"),
    )
    .profile()
    .unwrap();
    let names: Vec<&str> = profile
        .metrics()
        .iter()
        .flat_map(|m| std::iter::once(m).chain(m.nested()))
        .map(|m| m.name())
        .collect();
    assert!(
        names.contains(&r#"gql:value("n.name")"#),
        "{names:?}\n{profile}"
    );
}
