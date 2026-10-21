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
//!      neighbouring nodes in the written direction; a temporal value ([`temporal_text`])
//!      matches an expected string of exactly its text (so the offset is compared, and `Z` is
//!      not `+00:00`). Opaque values never match.

use std::collections::BTreeMap;

use std::fmt::Write as _;

use graphersal::ElementProperty;
use graphersal::storage::GraphStorage;
use graphersal::temporal::{Date, Duration, Temporal, Time};

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
        ElementProperty::Temporal(t) => match temporal_text(t) {
            Some(text) => TckValue::Temporal(text),
            None => TckValue::Opaque(format!("{t:?}")),
        },
        other => {
            let logical = other.logical();
            if matches!(logical.as_ref(), ElementProperty::String(_)) {
                // A compressed / deduplicated string read from storage.
                to_tck(graph, logical.as_ref())
            } else {
                // UUID, vector, a map with non-string keys: no TCK notation yet.
                TckValue::Opaque(format!("{other:?}"))
            }
        }
    }
}

/// The text of a temporal value as the TCK writes it: the `toString` of `java.time` (as Neo4j
/// prints its values), written here from the public getters only, independently of the
/// engine's own text and of GQL's `toString`, so the two check each other.
///
/// - a date `YYYY-MM-DD` (a `-` before years below 1, a `+` above 9999);
/// - a time `HH:MM`, then `:SS` only when the seconds or the fraction are not zero, then the
///   fraction in groups of three digits (`.645`, `.645876`; nine with nanoseconds, which our
///   values do not have);
/// - a local date-time `<date>T<time>`, a date-time with its offset `Z` / `+HH:MM[:SS]` (a
///   `datetime` keeps no zone name, so the `[Europe/Stockholm]` form is never written);
/// - a duration `P[nY][nM][nD][T[nH][nM][n.fS]]` (Neo4j's `DurationValue`): years and months
///   from the months, the days as they are, hours / minutes / seconds from the time part
///   without folding into days (`PT8784H`); every component carries its own sign, the seconds
///   signed together with their fraction (`PT-1M-0.001S`, `PT-1.999S`), the fraction without
///   trailing zeros (`PT23H59M59.9S`); `PT0S` when zero.
///
/// `None` for a kind this harness does not know yet (the enum is non-exhaustive).
pub fn temporal_text(value: &Temporal) -> Option<String> {
    let mut out = String::new();
    match value {
        Temporal::Date(d) => write_date(&mut out, *d),
        Temporal::Time(t) => write_time(&mut out, *t),
        Temporal::LocalDateTime(v) => {
            write_date(&mut out, v.date());
            out.push('T');
            write_time(&mut out, v.time());
        }
        Temporal::DateTime(v) => {
            let local = v.local();
            write_date(&mut out, local.date());
            out.push('T');
            write_time(&mut out, local.time());
            write_offset(&mut out, v.offset_seconds());
        }
        Temporal::Duration(d) => write_duration(&mut out, *d),
        _ => return None,
    }
    Some(out)
}

fn write_date(out: &mut String, d: Date) {
    let year = d.year();
    if year < 0 {
        let _ = write!(out, "-{:04}", -(year as i64));
    } else if year > 9999 {
        let _ = write!(out, "+{year}");
    } else {
        let _ = write!(out, "{year:04}");
    }
    let _ = write!(out, "-{:02}-{:02}", d.month(), d.day());
}

fn write_time(out: &mut String, t: Time) {
    let _ = write!(out, "{:02}:{:02}", t.hour(), t.minute());
    let nanos = t.microsecond() as u64 * 1000;
    if t.second() != 0 || nanos != 0 {
        let _ = write!(out, ":{:02}", t.second());
    }
    if nanos == 0 {
        return;
    }
    if nanos.is_multiple_of(1_000_000) {
        let _ = write!(out, ".{:03}", nanos / 1_000_000);
    } else if nanos.is_multiple_of(1000) {
        let _ = write!(out, ".{:06}", nanos / 1000);
    } else {
        let _ = write!(out, ".{nanos:09}");
    }
}

/// `ZoneOffset.getId()`: `Z`, `+HH:MM`, `+HH:MM:SS`.
fn write_offset(out: &mut String, seconds: i32) {
    if seconds == 0 {
        out.push('Z');
        return;
    }
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    let _ = write!(out, "{sign}{:02}:{:02}", abs / 3600, abs / 60 % 60);
    if !abs.is_multiple_of(60) {
        let _ = write!(out, ":{:02}", abs % 60);
    }
}

/// Neo4j's `DurationValue.prettyPrint` over (months, days, seconds, nanoseconds), the seconds
/// floored and the nanoseconds in `0..1e9` as Neo4j keeps them.
fn write_duration(out: &mut String, d: Duration) {
    let months = d.months() as i64;
    let days = d.days() as i64;
    let mut seconds = d.micros().div_euclid(1_000_000);
    let mut nanos = d.micros().rem_euclid(1_000_000) * 1000;
    if months == 0 && days == 0 && seconds == 0 && nanos == 0 {
        out.push_str("PT0S");
        return;
    }
    out.push('P');
    let part = |out: &mut String, value: i64, unit: char| {
        if value != 0 {
            let _ = write!(out, "{value}{unit}");
        }
    };
    part(out, months / 12, 'Y');
    part(out, months % 12, 'M');
    part(out, days, 'D');
    if seconds != 0 || nanos != 0 {
        // A negative time part is written with the fraction's sign on the seconds.
        if seconds < 0 && nanos != 0 {
            seconds += 1;
            nanos -= 1_000_000_000;
        }
        out.push('T');
        part(out, seconds / 3600, 'H');
        seconds %= 3600;
        part(out, seconds / 60, 'M');
        seconds %= 60;
        if seconds != 0 {
            let _ = write!(out, "{seconds}");
            write_nanos(out, nanos);
            out.push('S');
        } else if nanos != 0 {
            if nanos < 0 {
                out.push('-');
            }
            out.push('0');
            write_nanos(out, nanos);
            out.push('S');
        }
    }
}

/// `.` and the digits of `|nanos|` (nine places, trailing zeros dropped); nothing for zero.
fn write_nanos(out: &mut String, nanos: i64) {
    let n = nanos.unsigned_abs();
    if n == 0 {
        return;
    }
    let digits = format!("{n:09}");
    out.push('.');
    out.push_str(digits.trim_end_matches('0'));
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
        (TckValue::String(e) | TckValue::Temporal(e), TckValue::Temporal(a)) => e == a,
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
    use graphersal::temporal::{DateTime, LocalDateTime, TemporalKind};

    use super::super::values::to_element_property;

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

    fn text(value: impl Into<Temporal>) -> String {
        temporal_text(&value.into()).unwrap_or_default()
    }

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::from_ymd(y, m, d).unwrap_or_else(|e| panic!("{e}"))
    }

    fn time(h: u8, m: u8, s: u8, micro: u32) -> Time {
        Time::from_hms_micro(h, m, s, micro).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn temporal_text_of_dates_and_times() {
        assert_eq!(text(date(1984, 10, 11)), "1984-10-11");
        assert_eq!(text(date(1, 1, 1)), "0001-01-01");
        assert_eq!(text(date(0, 1, 1)), "0000-01-01");
        assert_eq!(text(date(-44, 3, 15)), "-0044-03-15");
        assert_eq!(text(date(-9999, 1, 1)), "-9999-01-01");
        assert_eq!(text(date(9999, 12, 31)), "9999-12-31");
        // Seconds only when they or the fraction are not zero; fractions in groups of three.
        assert_eq!(text(time(12, 0, 0, 0)), "12:00");
        assert_eq!(text(time(12, 31, 0, 0)), "12:31");
        assert_eq!(text(time(12, 31, 14, 0)), "12:31:14");
        assert_eq!(text(time(12, 31, 0, 1)), "12:31:00.000001");
        assert_eq!(text(time(12, 31, 14, 645_000)), "12:31:14.645");
        assert_eq!(text(time(12, 31, 14, 645_876)), "12:31:14.645876");
        assert_eq!(text(time(0, 0, 0, 100_000)), "00:00:00.100");
        let local = LocalDateTime::new(date(1984, 10, 11), time(12, 31, 0, 0));
        assert_eq!(text(local), "1984-10-11T12:31");
        let local = LocalDateTime::new(date(2015, 7, 21), time(21, 40, 32, 142_000));
        assert_eq!(text(local), "2015-07-21T21:40:32.142");
    }

    #[test]
    fn temporal_text_of_date_times_writes_the_offset() {
        let at = |offset: i32| {
            let local = LocalDateTime::new(date(1984, 10, 11), time(12, 31, 0, 0));
            text(DateTime::from_local(local, offset).unwrap_or_else(|e| panic!("{e}")))
        };
        assert_eq!(at(0), "1984-10-11T12:31Z");
        assert_eq!(at(3600), "1984-10-11T12:31+01:00");
        assert_eq!(at(-5400), "1984-10-11T12:31-01:30");
        assert_eq!(at(2 * 3600 + 5 * 60 + 59), "1984-10-11T12:31+02:05:59");
        assert_eq!(at(-(2 * 3600 + 5 * 60 + 7)), "1984-10-11T12:31-02:05:07");
        assert_eq!(at(18 * 3600), "1984-10-11T12:31+18:00");
        // The same instant at another offset is another text: the comparison sees the offset.
        let utc = TckValue::Temporal(at(0));
        assert!(matches(&TckValue::String(at(0)), &utc, S));
        assert!(!matches(&v("'1984-10-11T12:31+00:00'"), &utc, S));
        assert!(!matches(
            &v("'1984-10-11T13:31+01:00'"),
            &TckValue::Temporal(at(0)),
            S
        ));
    }

    #[test]
    fn temporal_text_of_durations_follows_the_tck_forms() {
        const SECOND: i64 = 1_000_000;
        let d = |months: i32, days: i32, micros: i64| text(Duration::new(months, days, micros));
        assert_eq!(d(0, 0, 0), "PT0S");
        assert_eq!(d(12, 0, 0), "P1Y");
        assert_eq!(d(-20, 0, 0), "P-1Y-8M");
        assert_eq!(d(0, -27, 0), "P-27D");
        assert_eq!(d(0, 0, 8784 * 3600 * SECOND), "PT8784H");
        assert_eq!(
            d(0, 0, (23 * 3600 + 59 * 60 + 59) * SECOND + 900_000),
            "PT23H59M59.9S"
        );
        assert_eq!(d(0, 0, 400_000), "PT0.4S");
        assert_eq!(d(0, 0, -(599 * SECOND + 400_000)), "PT-9M-59.4S");
        assert_eq!(
            d(0, -27, -((21 * 3600 + 40 * 60 + 32) * SECOND + 142_000)),
            "P-27DT-21H-40M-32.142S"
        );
        // Temporal6 [6]: the duration forms of `toString` (the nanosecond row needs ns).
        let rows: [(i32, i32, i64, &str); 10] = [
            (12 * 12 + 5, -14, 16 * 3600 * SECOND, "P12Y5M-14DT16H"),
            (0, 0, 12 * 60 * SECOND - 60 * SECOND, "PT11M"),
            (0, 0, 2 * SECOND - 1000, "PT1.999S"),
            (0, 0, -2 * SECOND + 1000, "PT-1.999S"),
            (0, 0, -2 * SECOND - 1000, "PT-2.001S"),
            (0, 1, 1000, "P1DT0.001S"),
            (0, 1, -1000, "P1DT-0.001S"),
            (0, 0, 60 * SECOND - 1000, "PT59.999S"),
            (0, 0, -60 * SECOND + 1000, "PT-59.999S"),
            (0, 0, -60 * SECOND - 1000, "PT-1M-0.001S"),
        ];
        for (months, days, micros, expected) in rows {
            assert_eq!(d(months, days, micros), expected);
        }
        assert_eq!(
            d(12 * 12 + 5, 14, ((16 * 60 + 13) * 60 + 10) * SECOND),
            "P12Y5M14DT16H13M10S"
        );
    }

    #[test]
    fn temporal_results_match_expected_strings_by_text() {
        let graph = TraversalGraph::new();
        let value = ElementProperty::Temporal(date(1984, 10, 11).into());
        assert_eq!(
            to_tck(&graph, &value),
            TckValue::Temporal("1984-10-11".into())
        );
        let actual = TckValue::Temporal("12:31".into());
        assert!(matches(&v("'12:31'"), &actual, S));
        assert!(!matches(&v("'12:31:00'"), &actual, S));
        assert!(matches(&actual, &actual, S));
        // Not a string the other way round, nor a number.
        assert!(!matches(&actual, &v("'12:31'"), S));
        assert!(!matches(&v("1231"), &actual, S));
        // Lists of temporals (Temporal4 stores lists) and node properties go through `to_tck`.
        let list = ElementProperty::Array(vec![
            value.clone(),
            ElementProperty::Temporal(Duration::new(0, 1, 0).into()),
        ]);
        assert!(matches(
            &v("['1984-10-11', 'P1D']"),
            &to_tck(&graph, &list),
            S
        ));
        assert!(to_element_property(&actual).is_err());
    }

    /// The temporal type a TCK text has, by its shape; `None` for any other string.
    fn temporal_shape(s: &str) -> Option<&'static str> {
        let b = s.as_bytes();
        let digits = |range: std::ops::Range<usize>| {
            b.get(range)
                .is_some_and(|r| r.iter().all(u8::is_ascii_digit))
        };
        if s.starts_with('P') || s.starts_with("-P") {
            return Some("duration");
        }
        let date_at = |i: usize| digits(i..i + 4) && b.get(i + 4) == Some(&b'-');
        if date_at(0) || ((s.starts_with('-') || s.starts_with('+')) && date_at(1)) {
            return Some(match s.find('T') {
                None => "date",
                Some(t) if s[t..].contains(['Z', '+', '-', '[']) => "datetime",
                Some(_) => "localdatetime",
            });
        }
        if digits(0..2) && b.get(2) == Some(&b':') {
            return Some(if s.contains(['Z', '+', '-']) {
                "time"
            } else {
                "localtime"
            });
        }
        None
    }

    /// Why a TCK text cannot be one of our values, or `None` when it can.
    fn unrepresentable(s: &str, shape: &str) -> Option<&'static str> {
        if s.contains('[') {
            return Some("a zone name");
        }
        if shape == "time" {
            return Some("a time with an offset");
        }
        // Digits below the microsecond.
        let mut rest = s;
        while let Some(at) = rest.find('.') {
            let fraction: String = rest[at + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if fraction.len() > 6 && fraction[6..].bytes().any(|c| c != b'0') {
                return Some("nanoseconds");
            }
            rest = &rest[at + 1..];
        }
        // Years beyond four digits, or a count beyond our fields.
        if s.split(|c: char| !c.is_ascii_digit()).any(|n| n.len() > 9)
            || (shape != "duration" && s.trim_start_matches(['-', '+']).find('-') > Some(4))
        {
            return Some("the range");
        }
        None
    }

    /// Every temporal text in a result table of `expressions/temporal` (outlines expanded):
    /// each one that our types can hold is read back (the engine's ISO parser), written by
    /// [`temporal_text`] and gives the same text; the others are counted by reason.
    #[test]
    fn every_temporal_cell_of_the_tree_renders_back() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/opencypher/features");
        let features = super::super::gherkin::load_all(&root).unwrap_or_else(|e| panic!("{e}"));
        let kind_of = |shape: &str| match shape {
            "date" => TemporalKind::Date,
            "localtime" => TemporalKind::Time,
            "localdatetime" => TemporalKind::LocalDateTime,
            "datetime" => TemporalKind::DateTime,
            _ => TemporalKind::Duration,
        };
        fn strings(value: &TckValue, out: &mut Vec<String>) {
            match value {
                TckValue::String(s) => out.push(s.clone()),
                TckValue::List(items) => items.iter().for_each(|i| strings(i, out)),
                TckValue::Map(map) => map.values().for_each(|i| strings(i, out)),
                TckValue::Node(n) => n.properties.values().for_each(|i| strings(i, out)),
                TckValue::Relationship(r) => r.properties.values().for_each(|i| strings(i, out)),
                _ => {}
            }
        }
        let mut rendered: BTreeMap<&str, usize> = BTreeMap::new();
        let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
        let mut failures = Vec::new();
        for feature in features
            .iter()
            .filter(|f| f.path.starts_with("expressions/temporal/"))
        {
            for case in feature.scenarios.iter().flat_map(|s| &s.cases) {
                for step in &case.steps {
                    let super::super::gherkin::Step::Result { table, .. } = &step.step else {
                        continue;
                    };
                    let mut texts = Vec::new();
                    for cell in table.rows.iter().flatten() {
                        match parse_value(cell) {
                            Ok(value) => strings(&value, &mut texts),
                            Err(e) => failures.push(format!("{}: {e}", case.id)),
                        }
                    }
                    for s in texts {
                        let Some(shape) = temporal_shape(&s) else {
                            continue;
                        };
                        if let Some(reason) = unrepresentable(&s, shape) {
                            *skipped.entry(reason).or_default() += 1;
                            continue;
                        }
                        match Temporal::parse_as(kind_of(shape), &s) {
                            Ok(t) if temporal_text(&t).as_deref() == Some(s.as_str()) => {
                                *rendered.entry(shape).or_default() += 1;
                            }
                            Ok(t) => failures.push(format!(
                                "{}: {s:?} renders as {:?}",
                                case.id,
                                temporal_text(&t)
                            )),
                            Err(e) => failures.push(format!("{}: {s:?}: {e}", case.id)),
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} failures:\n{}",
            failures.len(),
            failures.join("\n")
        );
        for shape in ["date", "localtime", "localdatetime", "datetime", "duration"] {
            assert!(
                rendered.get(shape).is_some_and(|n| *n >= 10),
                "too few {shape} texts: {rendered:?}, skipped {skipped:?}"
            );
        }
        for reason in ["a zone name", "a time with an offset", "nanoseconds"] {
            assert!(skipped.contains_key(reason), "{skipped:?}");
        }
    }

    #[test]
    fn unaliased_column_names_are_compared() {
        let graph = TraversalGraph::new();
        let result = run(&graph, "RETURN 1 + 2");
        compare_result(&table(&["1 + 2"], &[&["3"]]), RowOrder::Ordered, S, &result).unwrap();
    }
}
