//! The stage 3d write step `gql:delete` on hand-built pipelines (built by the lowering since
//! 3d.T3): `DETACH DELETE` of a hub, the `NODETACH` check per clause (G1001, nothing removed),
//! an edge and its ends in one clause, duplicates across rows, `NULL`, removed and non-element
//! targets, paths, nested lists, bulk, access requests equal to `drop()`, the profile; and the
//! reads of a deleted element in the GQL runtime (W27: an error, never a silent `NULL`; E3:
//! `type(r)` still answers; W28: a returned deleted element is an error).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::*;
use crate::error::{BoxedTraverserError, DiagnosticError};
use crate::gql::aggregate::compile_aggregating;
use crate::gql::eval::compile;
use crate::gql::lexer::Span;
use crate::gql::parser::parse;
use crate::graph::{AccessPolicy, AccessRequest, AccessRequestBuf, Authorizer, Denied};
use crate::traversal::TraversalStrategy;
use crate::traversal::step::flatbarrier_steps::step_gql_aggregate::GqlAggregateColumn;
use crate::traversal::step::flatbarrier_steps::step_gql_delete::GqlDeleteTarget;
use crate::traversal::step::map_steps::step_gql_project::GqlColumn;
use crate::{
    __, AccessFlow, AnonymousTraversal, GraphError, GraphStep, GraphTraversalSource,
    TraversalGraph, TraverserResult,
};

/// The variables of the tests: `n` the current object, `a` / `r` / `b` objects labelled so,
/// `x` a label never set (`NULL`), `p` the path `a`, `r`, `b`.
fn names() -> Vec<CompactString> {
    ["n", "a", "x", "r", "b", "p"]
        .iter()
        .map(|n| CompactString::from(*n))
        .collect()
}

fn bindings() -> Arc<[Binding]> {
    Arc::from([
        Binding::Current,
        Binding::Label("a".into()),
        Binding::Label("x".into()),
        Binding::Label("r".into()),
        Binding::Label("b".into()),
        Binding::Path(Arc::from([
            PathPart::Element("a".into()),
            PathPart::Element("r".into()),
            PathPart::Element("b".into()),
        ])),
    ])
}

fn scope(names: &[CompactString]) -> BindingScope<'_, impl Fn(&str) -> Option<ElementProperty>> {
    BindingScope {
        names,
        parameter: |_: &str| None,
    }
}

/// A target as the tests write it: its expression, its static kind and edge labels.
type Tg = (&'static str, Option<ElementKind>, &'static [&'static str]);

const NODE: Option<ElementKind> = Some(ElementKind::Vertex);
const EDGE: Option<ElementKind> = Some(ElementKind::Edge);

/// The query text of a clause: the targets as `RETURN` items (their compiled values and spans
/// point there), then the clause in a comment (the keyword span points there).
fn clause<S: GraphStorage>(
    keyword: &str,
    targets: &[Tg],
) -> (Arc<str>, Span, Vec<GqlDeleteTarget<S>>) {
    let items: Vec<String> = targets
        .iter()
        .enumerate()
        .map(|(i, (expr, ..))| format!("{expr} AS t{i}"))
        .collect();
    let exprs: Vec<&str> = targets.iter().map(|(expr, ..)| *expr).collect();
    let text = format!(
        "MATCH (n) RETURN {}\n-- {keyword} {}",
        items.join(", "),
        exprs.join(", ")
    );
    let parsed = parse(&text).unwrap_or_else(|e| panic!("{text:?} does not parse: {e}"));
    let start = text.rfind(keyword).unwrap();
    let keyword = Span {
        start,
        end: start + keyword.len(),
        line: 2,
        column: 4,
    };
    let names = names();
    let out = parsed
        .ret
        .items
        .iter()
        .zip(targets)
        .map(|(item, (_, element, labels))| GqlDeleteTarget {
            value: compile(&item.expr, &scope(&names)).unwrap(),
            element: *element,
            labels: labels.iter().map(|l| CompactString::from(*l)).collect(),
        })
        .collect();
    (Arc::from(text.as_str()), keyword, out)
}

/// `start.gql_delete(targets)`, `DETACH DELETE` when `detach`.
fn delete<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    detach: bool,
    targets: &[Tg],
) -> GraphTraversalSource<'g, S> {
    let keyword = if detach { "DETACH DELETE" } else { "DELETE" };
    let (query, keyword, targets) = clause(keyword, targets);
    start.gql_delete(query, keyword, detach, targets, bindings(), Vec::new())
}

/// The columns `exprs` (named `c0`, `c1`, ..) over the test variables.
fn columns<S: GraphStorage>(exprs: &[&str]) -> (Arc<str>, Vec<GqlColumn<S>>) {
    let items: Vec<String> = exprs
        .iter()
        .enumerate()
        .map(|(i, e)| format!("{e} AS c{i}"))
        .collect();
    let text = format!("MATCH (n) RETURN {}", items.join(", "));
    let parsed = parse(&text).unwrap_or_else(|e| panic!("{text:?} does not parse: {e}"));
    let names = names();
    let columns = parsed
        .ret
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| GqlColumn {
            name: format!("c{i}").into(),
            expr: compile(&item.expr, &scope(&names)).unwrap(),
        })
        .collect();
    (Arc::from(text.as_str()), columns)
}

/// `gql:project(exprs)` after `start`: a `WITH` row when `returning` is false, the query's
/// `RETURN` when true.
fn project<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    exprs: &[&str],
    returning: bool,
) -> GraphTraversalSource<'g, S> {
    let (query, columns) = columns(exprs);
    if returning {
        start.gql_project_returning(query, columns, bindings(), Vec::new())
    } else {
        start.gql_project_hosted(query, columns, bindings(), Vec::new())
    }
}

/// `gql:aggregate` of `exprs` after `start` (a column without an aggregate call is a key).
fn aggregate<'g, S: GraphStorage + 'g>(
    start: GraphTraversalSource<'g, S>,
    exprs: &[&str],
    returning: bool,
) -> GraphTraversalSource<'g, S> {
    let text = format!("MATCH (n) RETURN {}", exprs.join(", "));
    let parsed = parse(&text).unwrap();
    let names = names();
    let items = &parsed.ret.items;
    let compiled: crate::gql::aggregate::Aggregating<GqlElement<S>> =
        compile_aggregating(items.iter().map(|item| &item.expr), &scope(&names)).unwrap();
    let columns: Vec<GqlAggregateColumn<S>> = items
        .iter()
        .zip(compiled.outputs)
        .enumerate()
        .map(|(i, (item, expr))| {
            let alone: crate::gql::aggregate::Aggregating<GqlElement<S>> =
                compile_aggregating([&item.expr], &scope(&names)).unwrap();
            GqlAggregateColumn {
                name: format!("c{i}").into(),
                expr,
                key: alone.calls.is_empty(),
            }
        })
        .collect();
    let query: Arc<str> = Arc::from(text.as_str());
    if returning {
        start.gql_aggregate_returning(query, columns, compiled.calls, bindings(), Vec::new())
    } else {
        start.gql_aggregate_hosted(query, columns, compiled.calls, bindings(), Vec::new())
    }
}

fn root<T: std::fmt::Debug>(result: TraverserResult<T>) -> BoxedTraverserError {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error,
    }
}

fn gqlstatus(error: &TraverserError) -> String {
    match error.root_cause() {
        TraverserError::Gql { gqlstatus, .. } => gqlstatus.clone(),
        other => panic!("not a GQL error: {other}"),
    }
}

fn is_removed(error: &TraverserError) -> bool {
    matches!(
        error.root_cause(),
        TraverserError::Graph(GraphError::ElementRemoved { .. })
    )
}

fn counts(graph: &TraversalGraph) -> (usize, usize) {
    (graph.vertices().count(), graph.edges().count())
}

fn names_left(graph: &TraversalGraph) -> Vec<String> {
    let mut out: Vec<String> = graph
        .vertices()
        .filter_map(|v| {
            graph
                .vertex_properties(v)
                .and_then(|p| p.get("name"))
                .and_then(ElementProperty::as_string)
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out
}

/// A canonical dump of the whole graph.
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
            "e {:?} {:?} {:?}",
            graph.edge_label(e),
            graph.edge_endpoints(e).map(|(s, t)| (
                graph.vertex_id(s).map(str::to_string),
                graph.vertex_id(t).map(str::to_string)
            )),
            graph.edge_properties(e)
        ));
    }
    out.sort();
    out.join("\n")
}

fn named<'g>(
    graph: &'g mut TraversalGraph,
    name: &str,
) -> GraphTraversalSource<'g, &'g mut TraversalGraph> {
    graph.traversal_mut().v(None).has("name", name.to_string())
}

#[test]
fn detach_delete_removes_a_hub_with_its_edges() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // The row passes on (its current object now removed, so only a count reads it).
    let rows = delete(named(&mut graph, "marko"), true, &[("n", NODE, &[])])
        .count()
        .next()
        .unwrap();
    assert_eq!(rows, Some(ElementProperty::from(1)));
    assert_eq!(counts(&graph), (5, 3));
    assert!(!names_left(&graph).contains(&"marko".to_string()));
    // The same result as drop() (which takes the edges of a vertex with it).
    let mut twin = TraversalGraph::tinkerpop_modern();
    named(&mut twin, "marko").drop().iterate().unwrap();
    assert_eq!(dump(&graph), dump(&twin));
}

#[test]
fn nodetach_delete_of_a_node_with_edges_fails_and_removes_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = dump(&graph);
    // vadas (one edge) and marko: the clause fails on marko, and vadas stays too.
    let error = root(
        delete(
            graph
                .traversal_mut()
                .v(None)
                .has("name", "vadas")
                .as_("a")
                .in_(None),
            false,
            &[("a", NODE, &[]), ("n", NODE, &[])],
        )
        .iterate(),
    );
    assert_eq!(gqlstatus(&error), "G1001", "{error}");
    assert!(matches!(
        error.root_cause(),
        TraverserError::Gql {
            kind: GqlErrorKind::Data,
            ..
        }
    ));
    let help = error.help().unwrap_or_default();
    assert!(help.contains("DETACH DELETE"), "{help}");
    assert!(
        help.contains(r#"g.gql("MATCH (n) RETURN a AS t0, n AS t1\n-- DETACH DELETE a, n")"#),
        "{help}"
    );
    assert_eq!(dump(&graph), before);
}

#[test]
fn an_edge_and_its_ends_in_one_clause_pass_the_check() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // vadas <-knows- marko: deleting the edge and vadas in one plain DELETE works (W29, per
    // clause); marko keeps its other edges.
    delete(
        graph
            .traversal_mut()
            .v(None)
            .has("name", "vadas")
            .as_("b")
            .in_e(None)
            .as_("r"),
        false,
        &[("r", EDGE, &["knows"]), ("b", NODE, &[])],
    )
    .iterate()
    .unwrap();
    assert_eq!(counts(&graph), (5, 5));
    assert!(!names_left(&graph).contains(&"vadas".to_string()));
    // The edge alone in a later clause would not do: vadas has an edge, josh too.
    let mut graph = TraversalGraph::tinkerpop_modern();
    let error = root(delete(named(&mut graph, "vadas"), false, &[("n", NODE, &[])]).iterate());
    assert_eq!(gqlstatus(&error), "G1001");
    assert_eq!(counts(&graph), (6, 6));
}

#[test]
fn every_element_counts_once_whatever_the_rows() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // Every edge reached from both ends: 12 rows, 6 edges, each removed once; every row passes.
    let rows = delete(
        graph.traversal_mut().v(None).both_e(None).as_("r"),
        false,
        &[("n", EDGE, &[]), ("r", EDGE, &[])],
    )
    .count()
    .next()
    .unwrap();
    assert_eq!(rows, Some(ElementProperty::from(12)));
    assert_eq!(counts(&graph), (6, 0));
}

#[test]
fn null_and_removed_targets_are_ignored() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    // `x` is never set (an OPTIONAL MATCH that did not match): no-op, the row passes.
    let rows = delete(named(&mut graph, "marko"), false, &[("x", None, &[])])
        .to_list()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(counts(&graph), (6, 6));
    // NULL inside a list too.
    delete(named(&mut graph, "vadas"), true, &[("[x, n]", None, &[])])
        .iterate()
        .unwrap();
    assert_eq!(counts(&graph), (5, 5));
    // Removed earlier in the query (by drop()): ignored, no error, nothing else removed.
    let mut graph = TraversalGraph::tinkerpop_modern();
    delete(
        named(&mut graph, "marko")
            .as_("a")
            .side_effect_t(__::drop()),
        false,
        &[("a", NODE, &[])],
    )
    .iterate()
    .unwrap();
    assert_eq!(counts(&graph), (5, 3));
}

#[test]
fn paths_and_nested_lists_give_their_elements() {
    // p = marko -created-> lop: DETACH DELETE p removes both nodes and every edge of them.
    let mut graph = TraversalGraph::tinkerpop_modern();
    fn start(graph: &mut TraversalGraph) -> GraphTraversalSource<'_, &mut TraversalGraph> {
        graph
            .traversal_mut()
            .v(None)
            .has("name", "marko")
            .as_("a")
            .out_e("created")
            .as_("r")
            .in_v()
            .as_("b")
    }
    delete(start(&mut graph), true, &[("p", None, &[])])
        .iterate()
        .unwrap();
    assert_eq!(names_left(&graph), ["josh", "peter", "ripple", "vadas"]);
    assert_eq!(counts(&graph), (4, 1));
    // NODETACH DELETE p: marko and lop still have other edges.
    let mut graph = TraversalGraph::tinkerpop_modern();
    let error = root(delete(start(&mut graph), false, &[("p", None, &[])]).iterate());
    assert_eq!(gqlstatus(&error), "G1001");
    assert_eq!(counts(&graph), (6, 6));
    // A nested list: josh (a) and ripple (n) with their edges.
    let mut graph = TraversalGraph::tinkerpop_modern();
    delete(
        graph
            .traversal_mut()
            .v(None)
            .has("name", "josh")
            .as_("a")
            .out(None)
            .has("name", "ripple"),
        true,
        &[("[a, [x, [n]]]", None, &[])],
    )
    .iterate()
    .unwrap();
    assert_eq!(names_left(&graph), ["lop", "marko", "peter", "vadas"]);
    assert_eq!(counts(&graph), (4, 3));
}

#[test]
fn a_non_element_target_is_22g03() {
    for target in ["1 + 1", "{k: n}", "[n, 'a']", "n.name"] {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let error =
            root(delete(named(&mut graph, "vadas"), true, &[(target, None, &[])]).iterate());
        assert_eq!(gqlstatus(&error), "22G03", "{target}: {error}");
        assert!(error.to_string().contains(target), "{error}");
        let help = error.help().unwrap_or_default();
        assert!(help.contains("DETACH DELETE n"), "{help}");
        assert_eq!(counts(&graph), (6, 6), "{target}");
    }
}

#[test]
fn a_row_of_bulk_n_is_deleted_once_and_keeps_its_bulk() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let marko = graph
        .vertices()
        .find(|v| {
            graph
                .vertex_properties(*v)
                .and_then(|p| p.get("name"))
                .and_then(ElementProperty::as_string)
                == Some("marko")
        })
        .unwrap();
    let (query, keyword, targets) = clause("DETACH DELETE", &[("n", NODE, &[])]);
    let mut steps = AnonymousTraversal::<TraversalGraph>::new()
        .gql_delete(query, keyword, true, targets, bindings(), Vec::new())
        .steps;
    let GraphStep::FlatBarrier(_, step) = &mut steps[0] else {
        panic!("not a FlatBarrier");
    };
    let mut context = TraverserContext::new(&mut graph, TraversalStrategy::none(), HashMap::new());
    let mut source: Traverser<TraversalGraph> =
        Traverser::from_value(TraverserValue::Vertex(marko));
    source.bulk = 3;
    let rows = step.process(&mut context, vec![source]).unwrap().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bulk, 3);
    assert!(context.gql_deleted.any);
    // Its three edges are in the side table with their labels.
    let mut types: Vec<&str> = context
        .gql_deleted
        .edge_types
        .values()
        .map(CompactString::as_str)
        .collect();
    types.sort_unstable();
    assert_eq!(types, ["created", "knows", "knows"]);
    // No input: nothing removed, nothing emitted.
    assert!(
        step.process(&mut context, Vec::new())
            .unwrap()
            .unwrap()
            .is_empty()
    );
    drop(context);
    assert_eq!(counts(&graph), (5, 3));
}

#[test]
fn reads_of_a_deleted_element_are_errors_with_gql_help() {
    // W27: every read of the deleted node is the storage's ElementRemoved (openCypher
    // EntityNotFound), never a silent NULL / empty list / FALSE.
    for read in [
        "n.name",
        "labels(n)",
        "element_id(n)",
        "properties(n)",
        "keys(n)",
        "n:person",
        "n IS LABELED %",
    ] {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let error = root(
            project(
                delete(named(&mut graph, "marko"), true, &[("n", NODE, &[])]),
                &[read],
                false,
            )
            .iterate(),
        );
        assert!(is_removed(&error), "{read}: {error}");
        let help = error.help().unwrap_or_default();
        assert!(
            help.contains("WITH n, properties(n) AS p DETACH DELETE n RETURN p"),
            "{read}: {help}"
        );
        assert!(help.contains("deleted earlier in this query"), "{help}");
        // Nothing of the failed query stays.
        assert_eq!(counts(&graph), (6, 6), "{read}");
    }
    // What reads nothing still works: n IS NULL, the handle in a WITH row, count(n).
    let mut graph = TraversalGraph::tinkerpop_modern();
    let rows = project(
        delete(named(&mut graph, "marko"), true, &[("n", NODE, &[])]),
        &["n IS NULL", "n"],
        false,
    )
    .to_list();
    // The row map holds the removed handle: materializing it fails in the terminal (it is no
    // RETURN of the query), but `n IS NULL` evaluated.
    assert!(rows.is_err());
    let mut graph = TraversalGraph::tinkerpop_modern();
    let rows = project(
        delete(named(&mut graph, "marko"), true, &[("n", NODE, &[])]),
        &["n IS NULL"],
        true,
    )
    .to_list()
    .unwrap();
    assert_eq!(rows.len(), 1);
    let mut graph = TraversalGraph::tinkerpop_modern();
    let rows = aggregate(
        delete(
            graph.traversal_mut().v(None).has_label("person"),
            true,
            &[("n", NODE, &[])],
        ),
        &["count(n)"],
        true,
    )
    .to_list()
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], {
        let mut map = crate::PropertyMap::default();
        map.insert(CompactString::from("c0"), ElementProperty::from(4));
        ElementProperty::Object(Box::new(map))
    });
    // lop and ripple stay, without edges (each edge had a person end).
    assert_eq!(counts(&graph), (2, 0));
}

#[test]
fn returning_a_deleted_element_is_an_error() {
    // W28 (user decision 5): RETURN n, a list or a map holding it, collect(n).
    for (exprs, aggregated) in [
        (&["n"][..], false),
        (&["[1, n]"][..], false),
        (&["{k: [n]}"][..], false),
        (&["collect(n)"][..], true),
        (&["n", "count(*)"][..], true),
    ] {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let start = delete(named(&mut graph, "marko"), true, &[("n", NODE, &[])]);
        let result = if aggregated {
            aggregate(start, exprs, true).to_list()
        } else {
            project(start, exprs, true).to_list()
        };
        let error = root(result);
        assert!(is_removed(&error), "{exprs:?}: {error}");
        let help = error.help().unwrap_or_default();
        assert!(help.contains("properties(n) AS p"), "{help}");
        assert_eq!(counts(&graph), (6, 6));
    }
    // A RETURN of live elements after a DELETE: fine.
    let mut graph = TraversalGraph::tinkerpop_modern();
    let rows = project(
        delete(
            graph
                .traversal_mut()
                .v(None)
                .has("name", "vadas")
                .as_("b")
                .in_e(None)
                .as_("r")
                .out_v(),
            false,
            &[("r", EDGE, &[])],
        ),
        &["n", "n.name"],
        true,
    )
    .to_list()
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(counts(&graph), (6, 5));
}

#[test]
fn type_of_a_deleted_edge_still_answers() {
    // E3 (openCypher Return2 [14]): type(r) of an edge deleted earlier gives its label.
    let mut graph = TraversalGraph::tinkerpop_modern();
    let rows = project(
        delete(
            named(&mut graph, "marko").out_e(None).as_("r"),
            false,
            &[("r", EDGE, &[])],
        ),
        &["type(r)"],
        true,
    )
    .to_list()
    .unwrap();
    let mut types: Vec<String> = rows
        .iter()
        .map(|row| match row {
            ElementProperty::Object(map) => map
                .get("c0")
                .and_then(ElementProperty::as_string)
                .unwrap_or_default()
                .to_string(),
            other => panic!("not a row: {other:?}"),
        })
        .collect();
    types.sort();
    assert_eq!(types, ["created", "knows", "knows"]);
    // Also for the edges a DETACH DELETE took with its node.
    let mut graph = TraversalGraph::tinkerpop_modern();
    let rows = project(
        delete(
            named(&mut graph, "vadas").in_e(None).as_("r").in_v(),
            true,
            &[("n", NODE, &[])],
        ),
        &["type(r)"],
        true,
    )
    .to_list()
    .unwrap();
    assert_eq!(rows.len(), 1);
    // Every other read of the deleted edge is the W27 error.
    for read in [
        "r.weight",
        "properties(r)",
        "element_id(r)",
        "startNode(r)",
        "r:knows",
    ] {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let error = root(
            project(
                delete(
                    named(&mut graph, "marko").out_e(None).as_("r"),
                    false,
                    &[("r", EDGE, &[])],
                ),
                &[read],
                false,
            )
            .iterate(),
        );
        assert!(is_removed(&error), "{read}: {error}");
        assert!(
            error.help().unwrap_or_default().contains("type(r)"),
            "{read}"
        );
    }
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

fn recorder() -> Arc<Recorder> {
    Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
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
fn access_requests_equal_drop() {
    type Source<'g> = GraphTraversalSource<'g, &'g mut TraversalGraph>;
    type Start = fn(Source<'_>) -> Source<'_>;
    // (start, GQL target, detach, the Gremlin twin from the same start)
    let cases: Vec<(Start, Tg, bool)> = vec![
        (|g| g.v(None).has("name", "vadas"), ("n", NODE, &[]), true),
        (|g| g.v(None), ("n", NODE, &[]), true),
        (
            |g| g.e(None).has_label("knows"),
            ("n", EDGE, &["knows"]),
            false,
        ),
        (|g| g.e(None), ("n", EDGE, &[]), false),
        // Unknown kind (a list, a path, a map member): `Data { None, None }`, as `drop()`
        // after a step the plan does not state.
        (
            |g| g.v(None).has("name", "vadas").fold().unfold(),
            ("n", None, &[]),
            true,
        ),
    ];
    for (start, target, detach) in cases {
        let mut graph = TraversalGraph::tinkerpop_modern();
        let gql = recorder();
        delete(
            start(graph.traversal_mut().with_authorizer(gql.clone())),
            detach,
            &[target],
        )
        .iterate()
        .unwrap();
        let after_gql = dump(&graph);
        let mut graph = TraversalGraph::tinkerpop_modern();
        let gremlin = recorder();
        start(graph.traversal_mut().with_authorizer(gremlin.clone()))
            .drop()
            .iterate()
            .unwrap();
        assert_eq!(seen(&gql), seen(&gremlin), "{target:?}");
        assert!(seen(&gql).iter().any(|r| r.starts_with("Delete")));
        assert_eq!(after_gql, dump(&graph), "{target:?}");
    }
    // The requests come from the step's own target facts, never from the flow; one per
    // distinct (kind, label).
    let graph = TraversalGraph::tinkerpop_modern();
    let steps = delete(
        GraphTraversalSource::new(&graph),
        false,
        &[
            ("r", EDGE, &["knows", "created"]),
            ("n", NODE, &[]),
            ("a", NODE, &[]),
            ("p", None, &[]),
        ],
    )
    .take_steps();
    let flow = AccessFlow {
        element: Some(ElementKind::Vertex),
        labels: vec!["person".into()],
    };
    let mut requests = Vec::new();
    steps[0]
        .for_each_access_in(&flow, |request| {
            requests.push(request.to_string());
            Ok::<(), ()>(())
        })
        .unwrap();
    assert_eq!(
        requests,
        [
            r#"Delete Data(edge "knows")"#,
            r#"Delete Data(edge "created")"#,
            "Delete Data(vertex)",
            "Delete Data",
        ]
    );
    assert!(steps[0].mutates());
    // The rows pass on: the flow stays.
    assert_eq!(steps[0].access_flow_after(flow.clone()), flow);
}

#[test]
fn a_denied_delete_changes_nothing() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let before = dump(&graph);
    let error = root(
        delete(
            graph
                .traversal_mut()
                .with_authorizer(Arc::new(AccessPolicy::read_only()))
                .v(None),
            true,
            &[("n", NODE, &[])],
        )
        .iterate(),
    );
    assert!(
        matches!(error.root_cause(), TraverserError::PermissionDenied { .. }),
        "{error}"
    );
    assert_eq!(dump(&graph), before);
}

#[test]
fn profile_text_and_path_requirements() {
    let mut graph = TraversalGraph::tinkerpop_modern();
    let profile = delete(
        graph
            .traversal_mut()
            .v(None)
            .has("name", "vadas")
            .as_("b")
            .in_e(None)
            .as_("r"),
        false,
        &[("r", EDGE, &[]), ("b", NODE, &[])],
    )
    .profile()
    .unwrap();
    let names: Vec<String> = profile
        .metrics()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    assert!(
        names.contains(&r#"gql:delete("DELETE r, b")"#.to_string()),
        "{profile}"
    );
    let text = profile.to_string();
    assert!(text.contains("[path: labels(b,r)]"), "{text}");
    let steps = delete(
        GraphTraversalSource::new(&graph),
        true,
        &[("p", None, &[]), ("[a, n]", None, &[])],
    )
    .take_steps();
    assert_eq!(
        steps[0].get_name(),
        r#"gql:delete("DETACH DELETE p, [a, n]")"#
    );
    let GraphStep::FlatBarrier(_, step) = &steps[0] else {
        panic!("not a FlatBarrier");
    };
    assert_eq!(
        step.path_requirement(),
        crate::traversal::optimizer::path_analysis::PathRequirement::labels(
            ["a", "b", "r"].map(CompactString::from)
        )
    );
    drop(steps);
    // Under camel case the GQL text in the literal is not respelled.
    let camel = delete(
        graph
            .traversal_mut()
            .with("render.spelling", "camel")
            .v(None)
            .has("name", "vadas"),
        true,
        &[("coalesce(n, x)", None, &[])],
    )
    .profile()
    .unwrap()
    .to_string();
    assert!(camel.contains("gql:delete("), "{camel}");
    assert!(camel.contains("coalesce(n, x)"), "{camel}");
}
