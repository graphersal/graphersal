//! Comparison of a GQL query result with a TCK result table.
//!
//! Two phases, so the runner (3b.T4) can keep the graph lock short:
//!
//! 1. [`materialize`] turns the rows `gql()` / `gql_with()` returns (`Vec<ElementProperty>`, one
//!    `ElementProperty::Object` per row, its keys the column names in written order) into an
//!    [`ActualResult`] of [`TckValue`]s. Returned vertices and edges are read again through
//!    `GraphStorage` by their id (labels, properties and, for an edge, the endpoint ids); an
//!    element the graph no longer holds falls back to the snapshot in the value. The ids are kept
//!    only for path checks, never compared.
//! 2. [`compare_result`] compares an [`ActualResult`] with the expected [`Table`]:
//!    - column names: the header must equal the actual columns, in order (checked when the
//!      result has at least one row: an empty result carries no column names);
//!    - rows: in order for [`RowOrder::Ordered`], else as a multiset (bipartite matching, so a
//!      greedy mismatch cannot hide an existing assignment);
//!    - values ([`matches`]): integers and floats are distinct (`1` != `1.0`); two floats match
//!      within 1 ulp (as the TinkerPop harness: `NaN` equals `NaN`, infinities exact, `+0` equals
//!      `-0`); lists element by element, or as multisets at every depth for
//!      [`ListOrder::IgnoreElementOrder`]; maps by key set and values; nodes by label set and
//!      properties, relationships by type and properties (no ids); an expected path matches an
//!      actual list `[node, rel, node, ..]` (our path value) whose relationships connect the
//!      neighbouring nodes in the written direction. Opaque values never match.

use std::collections::BTreeMap;

use graphersal::ElementProperty;
use graphersal::storage::GraphStorage;

use super::gherkin::{ListOrder, RowOrder, Table};
use super::values::{Direction, TckNode, TckPath, TckRelationship, TckValue, parse_value};

/// A query result ready to compare.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ActualResult {
    /// Column names in result order (empty when there is no row).
    pub columns: Vec<String>,
    /// The rows, each in `columns` order.
    pub rows: Vec<Vec<TckValue>>,
}

/// Materializes the rows of a `gql()` result read from `graph` (the graph the query ran on,
/// after it ran). Every row must be an object with the same keys as the first.
pub fn materialize<S: GraphStorage>(
    graph: &S,
    rows: &[ElementProperty],
) -> Result<ActualResult, String> {
    let mut result = ActualResult::default();
    for (index, row) in rows.iter().enumerate() {
        let ElementProperty::Object(object) = row else {
            return Err(format!("row {} is not a column map: {row:?}", index + 1));
        };
        let columns: Vec<String> = object.iter().map(|(k, _)| k.to_string()).collect();
        if index == 0 {
            result.columns = columns;
        } else if columns != result.columns {
            return Err(format!(
                "row {} has columns {columns:?}, the first row {:?}",
                index + 1,
                result.columns
            ));
        }
        result
            .rows
            .push(object.iter().map(|(_, v)| to_tck(graph, v)).collect());
    }
    Ok(result)
}

/// One result value as a [`TckValue`] (see the module doc).
pub fn to_tck<S: GraphStorage>(graph: &S, value: &ElementProperty) -> TckValue {
    match value {
        ElementProperty::Null => TckValue::Null,
        ElementProperty::Boolean(b) => TckValue::Bool(*b),
        ElementProperty::String(s) => TckValue::String(s.to_string()),
        ElementProperty::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) if n.is_i64() => TckValue::Int(i),
            (_, Some(f)) => TckValue::Float(f),
            _ => TckValue::Opaque(format!("{n:?}")),
        },
        ElementProperty::Array(items) => {
            TckValue::List(items.iter().map(|v| to_tck(graph, v)).collect())
        }
        ElementProperty::Object(object) => TckValue::Map(
            object
                .iter()
                .map(|(k, v)| (k.to_string(), to_tck(graph, v)))
                .collect(),
        ),
        ElementProperty::Vertex(vertex) => {
            TckValue::Node(match graph.get_vertex_by_id(vertex.id()) {
                Some(handle) => TckNode {
                    labels: graph
                        .vertex_labels(handle)
                        .iter()
                        .map(|l| l.to_string())
                        .collect(),
                    properties: graph
                        .vertex_properties(handle)
                        .map(|props| {
                            props
                                .iter()
                                .map(|(k, v)| (k.to_string(), to_tck(graph, v)))
                                .collect()
                        })
                        .unwrap_or_default(),
                    id: Some(vertex.id().to_string()),
                },
                None => TckNode {
                    labels: vertex.labels().iter().map(|l| l.to_string()).collect(),
                    properties: vertex
                        .properties()
                        .iter()
                        .map(|(k, v)| (k.to_string(), to_tck(graph, v)))
                        .collect(),
                    id: Some(vertex.id().to_string()),
                },
            })
        }
        ElementProperty::Edge(edge) => {
            let snapshot_ends = (edge.out_v().id().to_string(), edge.in_v().id().to_string());
            TckValue::Relationship(match graph.get_edge_by_id(edge.id()) {
                Some(handle) => TckRelationship {
                    rel_type: graph.edge_label(handle).unwrap_or_default().to_string(),
                    properties: graph
                        .edge_properties(handle)
                        .map(|props| {
                            props
                                .iter()
                                .map(|(k, v)| (k.to_string(), to_tck(graph, v)))
                                .collect()
                        })
                        .unwrap_or_default(),
                    ends: Some(
                        graph
                            .edge_endpoints(handle)
                            .and_then(|(out_v, in_v)| {
                                Some((
                                    graph.vertex_id(out_v)?.to_string(),
                                    graph.vertex_id(in_v)?.to_string(),
                                ))
                            })
                            .unwrap_or(snapshot_ends),
                    ),
                },
                None => TckRelationship {
                    rel_type: edge.label().unwrap_or_default().to_string(),
                    properties: edge
                        .properties()
                        .iter()
                        .map(|(k, v)| (k.to_string(), to_tck(graph, v)))
                        .collect(),
                    ends: Some(snapshot_ends),
                },
            })
        }
        other => {
            let logical = other.logical();
            if matches!(logical.as_ref(), ElementProperty::String(_)) {
                // A compressed / deduplicated string read from storage.
                to_tck(graph, logical.as_ref())
            } else {
                // UUID, temporal, vector, a map with non-string keys: no TCK notation yet.
                TckValue::Opaque(format!("{other:?}"))
            }
        }
    }
}

/// Compares `actual` with the `expected` table (see the module doc). `Err` names the first
/// difference: a column mismatch, a row count, the row that has no partner, or a cell that does
/// not parse.
pub fn compare_result(
    expected: &Table,
    rows: RowOrder,
    lists: ListOrder,
    actual: &ActualResult,
) -> Result<(), String> {
    let expected_rows: Vec<Vec<TckValue>> = expected
        .rows
        .iter()
        .enumerate()
        .map(|(r, row)| {
            row.iter()
                .enumerate()
                .map(|(c, cell)| {
                    parse_value(cell)
                        .map_err(|e| format!("expected row {} column {}: {e}", r + 1, c + 1))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<_, _>>()?;
    if !actual.rows.is_empty() && actual.columns != expected.header {
        return Err(format!(
            "columns differ: expected {:?}, actual {:?}",
            expected.header, actual.columns
        ));
    }
    if expected_rows.len() != actual.rows.len() {
        return Err(format!(
            "expected {} rows, actual {}:\n{}",
            expected_rows.len(),
            actual.rows.len(),
            render_rows("actual", &actual.rows)
        ));
    }
    let row_matches = |e: &[TckValue], a: &[TckValue]| {
        e.len() == a.len() && e.iter().zip(a).all(|(e, a)| matches(e, a, lists))
    };
    match rows {
        RowOrder::Ordered => {
            for (i, (e, a)) in expected_rows.iter().zip(&actual.rows).enumerate() {
                if !row_matches(e, a) {
                    return Err(format!(
                        "row {} differs: expected {}, actual {}",
                        i + 1,
                        render_row(e),
                        render_row(a)
                    ));
                }
            }
            Ok(())
        }
        RowOrder::Unordered => {
            match unmatched(&expected_rows, &actual.rows, |e, a| row_matches(e, a)) {
                None => Ok(()),
                Some(i) => Err(format!(
                    "expected row {} {} has no partner (in any order) among:\n{}",
                    i + 1,
                    render_row(&expected_rows[i]),
                    render_rows("actual", &actual.rows)
                )),
            }
        }
    }
}

/// Whether the actual value matches the expectation (see the module doc).
pub fn matches(expected: &TckValue, actual: &TckValue, lists: ListOrder) -> bool {
    match (expected, actual) {
        (TckValue::Null, TckValue::Null) => true,
        (TckValue::Bool(e), TckValue::Bool(a)) => e == a,
        (TckValue::Int(e), TckValue::Int(a)) => e == a,
        (TckValue::Float(e), TckValue::Float(a)) => floats_within_one_ulp(*e, *a),
        (TckValue::String(e), TckValue::String(a)) => e == a,
        (TckValue::List(e), TckValue::List(a)) => match lists {
            ListOrder::Strict => {
                e.len() == a.len() && e.iter().zip(a).all(|(e, a)| matches(e, a, lists))
            }
            ListOrder::IgnoreElementOrder => {
                e.len() == a.len() && unmatched(e, a, |e, a| matches(e, a, lists)).is_none()
            }
        },
        (TckValue::Map(e), TckValue::Map(a)) => maps_match(e, a, lists),
        (TckValue::Node(e), TckValue::Node(a)) => nodes_match(e, a, lists),
        (TckValue::Relationship(e), TckValue::Relationship(a)) => rels_match(e, a, lists),
        (TckValue::Path(e), TckValue::List(a)) => path_matches(e, a, lists),
        (TckValue::Path(e), TckValue::Path(a)) => {
            nodes_match(&e.start, &a.start, lists)
                && e.hops.len() == a.hops.len()
                && e.hops
                    .iter()
                    .zip(&a.hops)
                    .all(|((er, ed, en), (ar, ad, an))| {
                        ed == ad && rels_match(er, ar, lists) && nodes_match(en, an, lists)
                    })
        }
        _ => false,
    }
}

fn maps_match(
    e: &BTreeMap<String, TckValue>,
    a: &BTreeMap<String, TckValue>,
    lists: ListOrder,
) -> bool {
    e.len() == a.len()
        && e.iter()
            .all(|(k, ev)| a.get(k).is_some_and(|av| matches(ev, av, lists)))
}

fn nodes_match(e: &TckNode, a: &TckNode, lists: ListOrder) -> bool {
    e.labels == a.labels && maps_match(&e.properties, &a.properties, lists)
}

fn rels_match(e: &TckRelationship, a: &TckRelationship, lists: ListOrder) -> bool {
    e.rel_type == a.rel_type && maps_match(&e.properties, &a.properties, lists)
}

/// An expected path against our path value, a list `[node, rel, node, ..]`: same shape, each
/// element matching, and each relationship joining its neighbours in the written direction.
fn path_matches(e: &TckPath, a: &[TckValue], lists: ListOrder) -> bool {
    if a.len() != 1 + 2 * e.hops.len() {
        return false;
    }
    let TckValue::Node(start) = &a[0] else {
        return false;
    };
    if !nodes_match(&e.start, start, lists) {
        return false;
    }
    let mut previous = start;
    for (i, (er, direction, en)) in e.hops.iter().enumerate() {
        let (TckValue::Relationship(ar), TckValue::Node(an)) = (&a[1 + 2 * i], &a[2 + 2 * i])
        else {
            return false;
        };
        if !rels_match(er, ar, lists) || !nodes_match(en, an, lists) {
            return false;
        }
        let (Some((source, target)), Some(prev_id), Some(next_id)) =
            (&ar.ends, &previous.id, &an.id)
        else {
            return false;
        };
        let joined = match direction {
            Direction::Forward => source == prev_id && target == next_id,
            Direction::Backward => source == next_id && target == prev_id,
        };
        if !joined {
            return false;
        }
        previous = an;
    }
    true
}

/// `true` when two `f64` values are equal or adjacent floats (at most 1 ulp apart).
///
/// Copied from the TinkerPop harness (W42): `NaN` equals `NaN` (and nothing else), infinities
/// compare exactly, `+0` equals `-0`, values of different sign are never equal otherwise.
pub fn floats_within_one_ulp(a: f64, b: f64) -> bool {
    if a.is_nan() || b.is_nan() {
        return a.is_nan() && b.is_nan();
    }
    if a == b {
        return true;
    }
    if a.is_infinite() || b.is_infinite() || a.is_sign_negative() != b.is_sign_negative() {
        return false;
    }
    a.to_bits().abs_diff(b.to_bits()) <= 1
}

/// Multiset matching of equally long `expected` and `actual` under `eq` (Kuhn's augmenting
/// paths after a greedy pass): `None` when every expected item has its own partner, else the
/// index of an expected item left without one.
fn unmatched<E, A>(expected: &[E], actual: &[A], eq: impl Fn(&E, &A) -> bool) -> Option<usize> {
    if expected.len() != actual.len() {
        return Some(0);
    }
    let n = expected.len();
    // partner_of_actual[j] = the expected index assigned to actual j.
    let mut partner_of_actual: Vec<Option<usize>> = vec![None; n];
    let mut assigned = vec![false; n];
    for (i, e) in expected.iter().enumerate() {
        if let Some(j) = (0..n).find(|&j| partner_of_actual[j].is_none() && eq(e, &actual[j])) {
            partner_of_actual[j] = Some(i);
            assigned[i] = true;
        }
    }
    if assigned.iter().all(|a| *a) {
        return None;
    }
    // Adjacency computed once for the augmenting phase.
    let adjacency: Vec<Vec<usize>> = expected
        .iter()
        .map(|e| (0..n).filter(|&j| eq(e, &actual[j])).collect())
        .collect();
    for i in (0..n).filter(|&i| !assigned[i]) {
        let mut visited = vec![false; n];
        if !augment(i, &adjacency, &mut partner_of_actual, &mut visited) {
            return Some(i);
        }
    }
    None
}

/// One augmenting-path search from expected `i` (iterative, so depth is bounded by the heap).
fn augment(
    i: usize,
    adjacency: &[Vec<usize>],
    partner_of_actual: &mut [Option<usize>],
    visited: &mut [bool],
) -> bool {
    // Stack of (expected index, next adjacency position, actual index taken to reach it).
    let mut stack: Vec<(usize, usize, Option<usize>)> = vec![(i, 0, None)];
    while let Some(top) = stack.last_mut() {
        let (e, pos, _) = *top;
        if pos >= adjacency[e].len() {
            stack.pop();
            continue;
        }
        top.1 += 1;
        let j = adjacency[e][pos];
        if visited[j] {
            continue;
        }
        visited[j] = true;
        match partner_of_actual[j] {
            None => {
                // Flip the path: the top frame takes `j`, every lower frame the actual through
                // which the frame above it was reached.
                let mut take = j;
                for &(expected, _, via) in stack.iter().rev() {
                    partner_of_actual[take] = Some(expected);
                    match via {
                        Some(via) => take = via,
                        None => break,
                    }
                }
                return true;
            }
            Some(other) => stack.push((other, 0, Some(j))),
        }
    }
    false
}

fn render_row(row: &[TckValue]) -> String {
    let cells: Vec<String> = row.iter().map(|v| v.to_string()).collect();
    format!("| {} |", cells.join(" | "))
}

fn render_rows(what: &str, rows: &[Vec<TckValue>]) -> String {
    let mut out = String::new();
    for row in rows.iter().take(50) {
        out.push_str("  ");
        out.push_str(&render_row(row));
        out.push('\n');
    }
    if rows.len() > 50 {
        out.push_str(&format!("  ... {} more {what} rows\n", rows.len() - 50));
    }
    out
}

#[cfg(test)]
mod tests {
    use graphersal::prelude::*;

    use super::*;

    fn table(header: &[&str], rows: &[&[&str]]) -> Table {
        Table {
            header: header.iter().map(|h| (*h).to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|c| (*c).to_string()).collect())
                .collect(),
        }
    }

    fn v(text: &str) -> TckValue {
        parse_value(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn actual(columns: &[&str], rows: &[&[&str]]) -> ActualResult {
        ActualResult {
            columns: columns.iter().map(|c| (*c).to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|c| v(c)).collect())
                .collect(),
        }
    }

    const S: ListOrder = ListOrder::Strict;
    const I: ListOrder = ListOrder::IgnoreElementOrder;

    #[test]
    fn numbers_keep_their_class_and_floats_use_one_ulp() {
        assert!(!matches(&v("1"), &v("1.0"), S));
        assert!(!matches(&v("1.0"), &v("1"), S));
        assert!(matches(&v("NaN"), &v("NaN"), S));
        assert!(!matches(&v("NaN"), &v("1.0"), S));
        assert!(matches(&v("-0.0"), &v("0.0"), S));
        assert!(matches(&v("Inf"), &v("Infinity"), S));
        assert!(!matches(&v("Inf"), &v("-Inf"), S));
        let up = f64::from_bits(0.1f64.to_bits() + 1);
        assert!(matches(&v("0.1"), &TckValue::Float(up), S));
        let up2 = f64::from_bits(0.1f64.to_bits() + 2);
        assert!(!matches(&v("0.1"), &TckValue::Float(up2), S));
        assert!(floats_within_one_ulp(f64::NAN, f64::NAN));
        assert!(!floats_within_one_ulp(5e-324, -5e-324));
    }

    #[test]
    fn lists_strict_and_ignoring_order_at_every_depth() {
        assert!(matches(&v("[1, [2, 3]]"), &v("[1, [2, 3]]"), S));
        assert!(!matches(&v("[1, 2]"), &v("[2, 1]"), S));
        assert!(matches(&v("[1, 2]"), &v("[2, 1]"), I));
        assert!(matches(&v("[[1, 2], 3]"), &v("[3, [2, 1]]"), I));
        assert!(!matches(&v("[1, 1, 2]"), &v("[1, 2, 2]"), I));
        assert!(!matches(&v("[1]"), &v("[1, 1]"), I));
        assert!(matches(&v("{a: [1, 2]}"), &v("{a: [2, 1]}"), I));
        assert!(!matches(&v("{a: [1, 2]}"), &v("{a: [2, 1]}"), S));
        assert!(!matches(&v("{a: 1}"), &v("{a: 1, b: null}"), S));
        assert!(matches(&v("{a: {b: [null]}}"), &v("{a: {b: [null]}}"), S));
    }

    #[test]
    fn elements_match_by_labels_type_and_properties() {
        assert!(matches(&v("(:A:B {n: 1})"), &v("(:B:A {n: 1})"), S));
        assert!(!matches(&v("(:A {n: 1})"), &v("(:A:B {n: 1})"), S));
        assert!(!matches(&v("(:A {n: 1})"), &v("(:A {n: 1.0})"), S));
        assert!(!matches(&v("(:A)"), &v("(:A {n: 1})"), S));
        assert!(matches(&v("[:T {n: 'x'}]"), &v("[:T {n: 'x'}]"), S));
        assert!(!matches(&v("[:T]"), &v("[:U]"), S));
        // Ids on the actual side are ignored.
        let mut node = match v("(:A)") {
            TckValue::Node(n) => n,
            _ => unreachable!(),
        };
        node.id = Some("7".into());
        assert!(matches(&v("(:A)"), &TckValue::Node(node), S));
        assert!(!matches(&v("(:A)"), &TckValue::Opaque("x".into()), S));
    }

    #[test]
    fn rows_ordered_and_as_multiset() {
        let expected = table(&["a", "b"], &[&["1", "'x'"], &["2", "'y'"]]);
        let same = actual(&["a", "b"], &[&["1", "'x'"], &["2", "'y'"]]);
        let swapped = actual(&["a", "b"], &[&["2", "'y'"], &["1", "'x'"]]);
        assert!(compare_result(&expected, RowOrder::Ordered, S, &same).is_ok());
        assert!(compare_result(&expected, RowOrder::Ordered, S, &swapped).is_err());
        assert!(compare_result(&expected, RowOrder::Unordered, S, &swapped).is_ok());
        // Duplicates count.
        let dup = table(&["a"], &[&["1"], &["1"], &["2"]]);
        let wrong = actual(&["a"], &[&["1"], &["2"], &["2"]]);
        assert!(compare_result(&dup, RowOrder::Unordered, S, &wrong).is_err());
        // Row count.
        let short = actual(&["a", "b"], &[&["1", "'x'"]]);
        assert!(compare_result(&expected, RowOrder::Unordered, S, &short).is_err());
        // Column names and their order.
        let renamed = actual(&["a", "c"], &[&["1", "'x'"], &["2", "'y'"]]);
        assert!(compare_result(&expected, RowOrder::Unordered, S, &renamed).is_err());
        let reordered = actual(&["b", "a"], &[&["'x'", "1"], &["'y'", "2"]]);
        assert!(compare_result(&expected, RowOrder::Unordered, S, &reordered).is_err());
        // An empty result has no column names to check.
        let none = table(&["a"], &[]);
        assert!(compare_result(&none, RowOrder::Unordered, S, &ActualResult::default()).is_ok());
        // A malformed expected cell is an error, not a pass.
        let bad = table(&["a"], &[&["[1"]]);
        let one = actual(&["a"], &[&["1"]]);
        assert!(compare_result(&bad, RowOrder::Unordered, S, &one).is_err());
    }

    #[test]
    fn multiset_matching_needs_augmenting_paths() {
        // Greedy pairs expected [1, 2] with the first actual [2, 1] under list-order-insensitive
        // rows only if lucky; build a case where greedy picks badly: expected e0 matches a0 and a1,
        // e1 only a0.
        let eq = |e: &u8, a: &u8| matches!((e, a), (0, 0) | (0, 1) | (1, 0));
        assert_eq!(unmatched(&[0u8, 1], &[0u8, 1], eq), None);
        let never = |e: &u8, a: &u8| *e == 0 && *a == 0;
        assert_eq!(unmatched(&[0u8, 0], &[0u8, 1], never), Some(1));
        // A longer chain: e_i matches a_i and a_{i+1}; greedy takes a_i for e_i except it is
        // forced to shift.
        let n = 40usize;
        let exp: Vec<usize> = (0..n).collect();
        let act: Vec<usize> = (0..n).collect();
        let chain = |e: &usize, a: &usize| *a == *e + 1 || (*e == n - 1 && *a == 0) || *a == *e;
        assert_eq!(unmatched(&exp, &act, chain), None);
        let reversed: Vec<usize> = (0..n).rev().collect();
        let picky = |e: &usize, a: &usize| *a == n - 1 - *e || (*e == 0 && *a == n - 1);
        assert_eq!(unmatched(&exp, &reversed, picky), None);
    }

    /// A small graph built with GQL writes, read back through `gql()` and materialized.
    fn run(graph: &TraversalGraph, query: &str) -> ActualResult {
        let rows = graph
            .traversal()
            .gql(query)
            .to_list()
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        materialize(graph, &rows).unwrap_or_else(|e| panic!("{query}: {e}"))
    }

    fn sample() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        graph
            .traversal_mut()
            .gql(
                "INSERT (:A:X {name: 'A'})-[:KNOWS {num: 1}]->(:B {name: 'B', w: 1.5})\
                 <-[:LIKES]-(:C {name: 'C'})",
            )
            .iterate()
            .unwrap_or_else(|e| panic!("setup: {e}"));
        graph
    }

    #[test]
    fn materialized_elements_match_the_notation() {
        let graph = sample();
        let result = run(
            &graph,
            "MATCH (a:A)-[r]->(b) RETURN a, r, b.w AS w, [b.name, 1] AS xs",
        );
        assert_eq!(result.columns, ["a", "r", "w", "xs"]);
        let expected = table(
            &["a", "r", "w", "xs"],
            &[&["(:X:A {name: 'A'})", "[:KNOWS {num: 1}]", "1.5", "['B', 1]"]],
        );
        compare_result(&expected, RowOrder::Ordered, S, &result).unwrap();
        let wrong = table(
            &["a", "r", "w", "xs"],
            &[&["(:A {name: 'A'})", "[:KNOWS {num: 1}]", "1.5", "['B', 1]"]],
        );
        assert!(compare_result(&wrong, RowOrder::Ordered, S, &result).is_err());
    }

    #[test]
    fn paths_match_in_both_directions() {
        let graph = sample();
        let forward = run(&graph, "MATCH p = (:A)-[:KNOWS]->(:B) RETURN p");
        let ok = table(
            &["p"],
            &[&["<(:A:X {name: 'A'})-[:KNOWS {num: 1}]->(:B {name: 'B', w: 1.5})>"]],
        );
        compare_result(&ok, RowOrder::Unordered, S, &forward).unwrap();
        // The same elements written in the wrong direction do not match.
        let reversed = table(
            &["p"],
            &[&["<(:A:X {name: 'A'})<-[:KNOWS {num: 1}]-(:B {name: 'B', w: 1.5})>"]],
        );
        assert!(compare_result(&reversed, RowOrder::Unordered, S, &forward).is_err());
        // The direction comes from the relationship's ends: swapped ends match the backward
        // notation and no longer the forward one.
        let swapped = reversed_actual(&forward);
        compare_result(&reversed, RowOrder::Unordered, S, &swapped).unwrap();
        assert!(compare_result(&ok, RowOrder::Unordered, S, &swapped).is_err());

        let backward = run(&graph, "MATCH p = (:B)<-[:KNOWS]-(:A) RETURN p");
        let ok = table(
            &["p"],
            &[&["<(:B {name: 'B', w: 1.5})<-[:KNOWS {num: 1}]-(:A:X {name: 'A'})>"]],
        );
        compare_result(&ok, RowOrder::Unordered, S, &backward).unwrap();

        let mixed = run(
            &graph,
            "MATCH p = (:A)-[:KNOWS]->(:B)<-[:LIKES]-(:C) RETURN p",
        );
        let ok = table(
            &["p"],
            &[&[
                "<(:A:X {name: 'A'})-[:KNOWS {num: 1}]->(:B {name: 'B', w: 1.5})<-[:LIKES]-(:C {name: 'C'})>",
            ]],
        );
        compare_result(&ok, RowOrder::Unordered, S, &mixed).unwrap();
        let single = run(&graph, "MATCH p = (:C) RETURN p");
        compare_result(
            &table(&["p"], &[&["<(:C {name: 'C'})>"]]),
            RowOrder::Unordered,
            S,
            &single,
        )
        .unwrap();
    }

    /// The forward result with the relationship's ends swapped (a backward edge).
    fn reversed_actual(result: &ActualResult) -> ActualResult {
        let mut out = result.clone();
        for row in &mut out.rows {
            for cell in row {
                if let TckValue::List(items) = cell {
                    for item in items {
                        if let TckValue::Relationship(rel) = item {
                            rel.ends = rel.ends.take().map(|(s, t)| (t, s));
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn floats_from_queries_keep_class_nan_and_negative_zero() {
        let graph = TraversalGraph::new();
        let result = run(&graph, "RETURN 1 AS i, 1.0 AS f, -0.0 AS z, 1e308 AS big");
        compare_result(
            &table(&["i", "f", "z", "big"], &[&["1", "1.0", "-0.0", "1e308"]]),
            RowOrder::Ordered,
            S,
            &result,
        )
        .unwrap();
        assert!(
            compare_result(
                &table(&["i", "f", "z", "big"], &[&["1.0", "1", "-0.0", "1e308"]]),
                RowOrder::Ordered,
                S,
                &result,
            )
            .is_err()
        );
        let nan = ActualResult {
            columns: vec!["x".into()],
            rows: vec![vec![TckValue::Float(f64::NAN)]],
        };
        compare_result(&table(&["x"], &[&["NaN"]]), RowOrder::Ordered, S, &nan).unwrap();
    }

    #[test]
    fn unaliased_column_names_are_compared() {
        let graph = TraversalGraph::new();
        let result = run(&graph, "RETURN 1 + 2");
        compare_result(&table(&["1 + 2"], &[&["3"]]), RowOrder::Ordered, S, &result).unwrap();
    }
}
