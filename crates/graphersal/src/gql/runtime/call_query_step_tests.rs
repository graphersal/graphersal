//! The stage 3h internal step `gql:call_query` on hand-built pipelines: the records of the
//! child under the yielded names, a child without records (VOID), the fresh seed (no path of
//! the caller, the unit row `0`), bulk, the path after it, the profile text and its `Execute`
//! request.

use super::*;
use crate::{__, AnonymousTraversal, ElementProperty, TraversalGraph};

/// A child giving the records `{v: 1}` and `{v: 2}`... as `project("v", "w")` of each value
/// (both columns hold it).
fn child<'g>() -> AnonymousTraversal<'g, &'g TraversalGraph> {
    __::union(vec![__::constant(1), __::constant(2)])
        .project(vec!["v".to_string(), "w".to_string()])
}

fn call<'g>(
    child: AnonymousTraversal<'g, &'g TraversalGraph>,
    outputs: &[(&str, usize)],
    unit: bool,
) -> GqlCallQuery<'g, &'g TraversalGraph> {
    GqlCallQuery::new(
        "q".into(),
        "t/q".into(),
        child,
        outputs.iter().map(|(n, c)| ((*n).into(), *c)).collect(),
        unit,
    )
}

fn text(value: &ElementProperty) -> String {
    format!("{value:?}")
}

#[test]
fn every_record_of_the_child_under_the_yielded_names() {
    let graph = TraversalGraph::new();
    let got = graph
        .traversal()
        .inject(vec![ElementProperty::from(7)])
        .gql_call_query(call(child(), &[("b", 1), ("a", 0)], false))
        .to_list()
        .unwrap();
    let got: Vec<String> = got.iter().map(text).collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got[0].contains("\"b\": 1") && got[0].contains("\"a\": 1"),
        "{got:?}"
    );
    assert!(got[1].contains("\"b\": 2"), "{got:?}");
}

#[test]
fn a_unit_call_emits_nothing() {
    let graph = TraversalGraph::new();
    let got = graph
        .traversal()
        .inject(vec![ElementProperty::from(7)])
        .gql_call_query(call(child(), &[], true))
        .to_list()
        .unwrap();
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn the_child_starts_on_a_fresh_unit_row() {
    let graph = TraversalGraph::new();
    // The seed is the unit row `0`, not the incoming value.
    let seed: AnonymousTraversal<'_, &TraversalGraph> =
        __::identity().project(vec!["v".to_string()]);
    let got = graph
        .traversal()
        .inject(vec![ElementProperty::from(7)])
        .gql_call_query(call(seed, &[("v", 0)], false))
        .select("v")
        .to_list()
        .unwrap();
    assert_eq!(got, [ElementProperty::from(0)]);
    // The caller's labels are not on the child's path.
    let reader: AnonymousTraversal<'_, &TraversalGraph> =
        __::select("a").project(vec!["v".to_string()]);
    let got = graph
        .traversal()
        .inject(vec![ElementProperty::from(7)])
        .as_("a")
        .gql_call_query(call(reader, &[("v", 0)], false))
        .to_list()
        .unwrap();
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn records_continue_the_incoming_row_with_its_bulk() {
    let graph = TraversalGraph::new();
    // Three equal rows merged into one of bulk 3: six records.
    let count = graph
        .traversal()
        .inject(vec![
            ElementProperty::from(5),
            ElementProperty::from(5),
            ElementProperty::from(5),
        ])
        .barrier()
        .gql_call_query(call(child(), &[("v", 0)], false))
        .count()
        .next()
        .unwrap();
    assert_eq!(count, Some(ElementProperty::from(6)));
    // The incoming path stays readable after it.
    let got = graph
        .traversal()
        .inject(vec![ElementProperty::from(7)])
        .as_("a")
        .gql_call_query(call(child(), &[("v", 0)], false))
        .select("a")
        .to_list()
        .unwrap();
    assert_eq!(got, [ElementProperty::from(7), ElementProperty::from(7)]);
}

#[test]
fn it_renders_its_name_and_child_and_asks_execute() {
    let graph = TraversalGraph::new();
    let source = graph
        .traversal()
        .inject(vec![ElementProperty::from(7)])
        .gql_call_query(call(child(), &[("v", 0)], false));
    let step = source.steps.last().expect("the step");
    let rendered = step.get_name();
    assert!(
        rendered.starts_with("gql:call_query(\"t/q\", __.union("),
        "{rendered}"
    );
    let mut requests = Vec::new();
    step.for_each_access(|request| {
        requests.push(request.to_owned_request().to_string());
        Ok::<(), ()>(())
    })
    .unwrap();
    assert_eq!(requests, [r#"Execute Definition(query "q")"#]);
}
