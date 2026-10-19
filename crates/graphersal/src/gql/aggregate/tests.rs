//! The pure aggregation stage: compiling aggregating items, every accumulator (NULL, bulk,
//! empty input, overflow, mixed kinds, `DISTINCT`), the grouping equality and path values.

use std::collections::HashSet;

use super::*;
use crate::gql::eval::Truth;
use crate::gql::parser::parse;

// ----- a mock row and graph -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum El {
    V(u32),
    E(u32),
}

/// Variables `n` (vertex 1), `e` (edge 7), `p` (the path v1 -e7-> v2), `x` (NULL), `k` (2).
struct Row;

const VARIABLES: &[&str] = &["n", "e", "p", "x", "k"];

impl Env for Row {
    type Element = El;

    fn variable(&self, slot: usize) -> Value<El> {
        match slot {
            0 => Value::Element(El::V(1)),
            1 => Value::Element(El::E(7)),
            2 => Value::Path(vec![El::V(1), El::E(7), El::V(2)]),
            4 => int(2),
            _ => Value::Null,
        }
    }

    fn kind(&self, element: &El) -> ElementKind {
        match element {
            El::V(_) => ElementKind::Vertex,
            El::E(_) => ElementKind::Edge,
        }
    }

    fn element_id(&self, element: &El) -> CompactString {
        match element {
            El::V(id) | El::E(id) => id.to_string().into(),
        }
    }

    fn property(&self, element: &El, key: &str) -> Option<ElementProperty> {
        match (element, key) {
            (El::V(1), "age") => Some(29.into()),
            _ => None,
        }
    }

    fn has_label(&self, _: &El, _: &str) -> bool {
        false
    }

    fn has_any_label(&self, _: &El) -> bool {
        false
    }

    fn labels(&self, _: &El) -> Vec<CompactString> {
        Vec::new()
    }
}

struct Names;

impl Scope for Names {
    fn variable(&self, name: &str) -> Option<usize> {
        VARIABLES.iter().position(|v| *v == name)
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        (name == "ten").then(|| ElementProperty::from(10))
    }
}

fn int(value: i64) -> Value<El> {
    Value::Scalar(ElementProperty::Number(Number::from_i64(value)))
}

fn float(value: f64) -> Value<El> {
    Value::Scalar(ElementProperty::Number(Number::from_f64(value)))
}

fn string(text: &str) -> Value<El> {
    Value::Scalar(ElementProperty::String(text.into()))
}

fn items(text: &str) -> Vec<Expr> {
    let query = format!("MATCH (n) RETURN {text}");
    match parse(&query) {
        Ok(parsed) => parsed.ret.items.into_iter().map(|item| item.expr).collect(),
        Err(e) => panic!("{query:?} does not parse: {e}"),
    }
}

fn aggregating(text: &str) -> Result<Aggregating<El>, EvalError> {
    compile_aggregating(&items(text), &Names)
}

fn span() -> Span {
    match parse("MATCH (n) RETURN n") {
        Ok(parsed) => parsed.ret.items[0].expr.span,
        Err(e) => panic!("{e}"),
    }
}

/// `function` over `(value, bulk)` rows.
fn run(
    function: Aggregate,
    distinct: bool,
    rows: &[(Value<El>, u64)],
) -> Result<Value<El>, EvalError> {
    let mut accumulator = Accumulator::new(function, distinct, span());
    for (value, bulk) in rows {
        accumulator.add(value.clone(), *bulk, &Row)?;
    }
    accumulator.finish()
}

fn value(function: Aggregate, distinct: bool, rows: &[(Value<El>, u64)]) -> Value<El> {
    match run(function, distinct, rows) {
        Ok(value) => value,
        Err(e) => panic!("{function:?} failed: {e:?}"),
    }
}

fn status_of(function: Aggregate, rows: &[(Value<El>, u64)]) -> &'static str {
    match run(function, false, rows) {
        Ok(value) => panic!("{function:?} gave {value:?}"),
        Err(e) => e.gqlstatus,
    }
}

fn one(value: Value<El>) -> (Value<El>, u64) {
    (value, 1)
}

// ----- compiling -----

#[test]
fn aggregates_compile_into_calls_and_results() {
    let compiled = match aggregating("n, count(*), count(n.age) + 1, sum(DISTINCT k) AS s") {
        Ok(compiled) => compiled,
        Err(e) => panic!("{e:?}"),
    };
    assert_eq!(compiled.outputs.len(), 4);
    let shape: Vec<(Aggregate, bool, bool)> = compiled
        .calls
        .iter()
        .map(|call| (call.aggregate, call.distinct, call.argument.is_some()))
        .collect();
    assert_eq!(
        shape,
        [
            (Aggregate::CountRows, false, false),
            (Aggregate::Count, false, true),
            (Aggregate::Sum, true, true),
        ]
    );
    // An output reads its aggregate's result, nothing else about it.
    let results = [int(5), int(3), int(2)];
    let env = WithAggregates {
        env: &Row,
        results: &results,
    };
    let values: Vec<Value<El>> = compiled
        .outputs
        .iter()
        .map(|output| output.eval(&env).unwrap_or(Value::Null))
        .collect();
    assert_eq!(values, [Value::Element(El::V(1)), int(5), int(4), int(2)]);
    // The argument's variables belong to the call, not to the output.
    let mut slots = Vec::new();
    compiled.outputs[2].variables(&mut slots);
    assert!(slots.is_empty());
    if let Some(argument) = &compiled.calls[1].argument {
        argument.variables(&mut slots);
    }
    assert_eq!(slots, [0]);
}

#[test]
fn aggregate_calls_are_driven_per_row() {
    let compiled = match aggregating("count(*), count(x), collect_list(n.age)") {
        Ok(compiled) => compiled,
        Err(e) => panic!("{e:?}"),
    };
    let mut accumulators: Vec<_> = compiled.calls.iter().map(|c| c.accumulator()).collect();
    for (call, accumulator) in compiled.calls.iter().zip(&mut accumulators) {
        for bulk in [2, 1] {
            if let Err(e) = call.add(accumulator, &Row, bulk) {
                panic!("{e:?}");
            }
        }
    }
    let results: Vec<Value<El>> = accumulators
        .into_iter()
        .map(|a| a.finish().unwrap_or(Value::Null))
        .collect();
    assert_eq!(
        results,
        [int(3), int(0), Value::List(vec![int(29), int(29), int(29)])]
    );
}

#[test]
fn compile_errors() {
    let status = |text: &str| match aggregating(text) {
        Ok(_) => panic!("{text:?} compiled"),
        Err(e) => (e.gqlstatus, e.message),
    };
    let (code, message) = status("count(sum(k))");
    assert_eq!(code, "42002");
    assert!(
        message.contains("inside the argument of another aggregate"),
        "{message}"
    );
    assert_eq!(status("count(missing)").0, "42002");
    // Outside an aggregating projection an aggregate stays refused.
    let plain = compile::<El>(&items("count(n)")[0], &Names);
    assert_eq!(plain.map(|_| ()).map_err(|e| e.gqlstatus), Err("0A000"));
}

// ----- accumulators -----

#[test]
fn count_skips_null_and_adds_bulk() {
    use Aggregate::*;
    let rows = [
        (int(1), 3),
        one(Value::Null),
        (string("a"), 2),
        (Value::Null, 4),
    ];
    assert_eq!(value(Count, false, &rows), int(5));
    assert_eq!(value(CountRows, false, &rows), int(10));
    assert_eq!(value(Count, true, &rows), int(2));
}

#[test]
fn sum_keeps_integers_and_weighs_by_bulk() {
    use Aggregate::*;
    assert_eq!(
        value(Sum, false, &[(int(2), 3), one(int(4)), one(Value::Null)]),
        int(10)
    );
    assert_eq!(
        value(Sum, false, &[one(int(1)), (float(0.5), 2)]),
        float(2.0)
    );
    // DISTINCT ignores the bulk; 1 and 1.0 are one value, the first arrival counts.
    assert_eq!(
        value(Sum, true, &[(int(1), 5), one(float(1.0)), one(int(2))]),
        int(3)
    );
}

#[test]
fn sum_overflow_and_wrong_kinds() {
    use Aggregate::*;
    assert_eq!(status_of(Sum, &[one(int(i64::MAX)), one(int(1))]), "22003");
    assert_eq!(status_of(Sum, &[(int(i64::MAX), 2)]), "22003");
    // The running total may leave i64 when the result fits.
    assert_eq!(
        value(
            Sum,
            false,
            &[one(int(i64::MAX)), one(int(i64::MAX)), (int(-i64::MAX), 2)]
        ),
        int(0)
    );
    assert_eq!(
        status_of(Sum, &[one(float(f64::MAX)), one(float(f64::MAX))]),
        "22003"
    );
    assert_eq!(status_of(Sum, &[one(int(1)), one(string("x"))]), "22G03");
    assert_eq!(status_of(Avg, &[one(Value::Element(El::V(1)))]), "22G03");
    match run(Sum, false, &[one(Value::List(vec![]))]) {
        Err(e) => assert_eq!(e.message, "sum() takes numbers, not a list"),
        Ok(v) => panic!("{v:?}"),
    }
    // A non-finite input gives a non-finite result, no overflow error.
    match value(Sum, false, &[one(float(f64::INFINITY)), one(int(1))]) {
        Value::Scalar(ElementProperty::Number(n)) => assert_eq!(n.as_f64(), Some(f64::INFINITY)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn avg_is_always_a_float() {
    use Aggregate::*;
    assert_eq!(value(Avg, false, &[one(int(1)), (int(2), 3)]), float(1.75));
    assert_eq!(
        value(Avg, false, &[one(int(2)), one(int(4)), one(Value::Null)]),
        float(3.0)
    );
    assert_eq!(
        value(Avg, false, &[one(int(1)), one(float(2.0))]),
        float(1.5)
    );
    assert_eq!(
        value(Avg, true, &[(int(2), 9), one(float(4.0))]),
        float(3.0)
    );
    // Exact beyond f64's integer range.
    assert_eq!(
        value(Avg, false, &[one(int(i64::MAX)), one(int(i64::MAX))]),
        float(i64::MAX as f64)
    );
    assert_eq!(value(Avg, false, &[(int(-3), 2)]), float(-3.0));
}

#[test]
fn min_and_max_order_across_kinds() {
    use Aggregate::*;
    let rows = [
        one(int(3)),
        one(Value::Null),
        one(string("b")),
        (float(2.5), 7),
        one(string("a")),
        one(Value::List(vec![int(1)])),
    ];
    // List < string < number; NULL skipped.
    assert_eq!(value(Min, false, &rows), Value::List(vec![int(1)]));
    assert_eq!(value(Max, false, &rows), int(3));
    assert_eq!(value(Min, false, &rows[1..5]), string("a"));
    assert_eq!(value(Max, true, &rows[1..5]), float(2.5));
    // Elements by id; a vertex before an edge.
    let elements = [one(Value::Element(El::E(1))), one(Value::Element(El::V(9)))];
    assert_eq!(value(Min, false, &elements), Value::Element(El::V(9)));
    assert_eq!(value(Max, false, &elements), Value::Element(El::E(1)));
    // The first of equal values stays.
    assert_eq!(value(Min, false, &[one(int(1)), one(float(1.0))]), int(1));
}

#[test]
fn collect_list_keeps_arrival_order_and_bulk() {
    use Aggregate::*;
    let rows = [
        (int(1), 2),
        one(Value::Null),
        one(string("a")),
        one(float(1.0)),
        one(int(1)),
    ];
    assert_eq!(
        value(CollectList, false, &rows),
        Value::List(vec![int(1), int(1), string("a"), float(1.0), int(1)])
    );
    let mut accumulator = Accumulator::new(CollectList, false, span());
    for (value, bulk) in &rows {
        let _ = accumulator.add(value.clone(), *bulk, &Row);
    }
    assert_eq!(accumulator.collected(), 5);
    // DISTINCT: the first arrival of each value under the grouping equality.
    assert_eq!(
        value(CollectList, true, &rows),
        Value::List(vec![int(1), string("a")])
    );
}

#[test]
fn empty_input_values() {
    use Aggregate::*;
    for distinct in [false, true] {
        assert_eq!(value(CountRows, distinct, &[]), int(0));
        assert_eq!(value(Count, distinct, &[one(Value::Null)]), int(0));
        assert_eq!(value(Sum, distinct, &[one(Value::Null)]), int(0));
        assert_eq!(value(CollectList, distinct, &[]), Value::List(vec![]));
        assert_eq!(value(Avg, distinct, &[one(Value::Null)]), Value::Null);
        assert_eq!(value(Min, distinct, &[]), Value::Null);
        assert_eq!(value(Max, distinct, &[one(Value::Null)]), Value::Null);
    }
}

// ----- grouping equality -----

fn key(value: Value<El>) -> GroupKey<El> {
    GroupKey(value)
}

fn distinct(values: Vec<Value<El>>) -> usize {
    values.into_iter().map(key).collect::<HashSet<_>>().len()
}

#[test]
fn grouping_equality_is_gql_equality() {
    let map = |entries: &[(&str, Value<El>)]| {
        Value::Map(
            entries
                .iter()
                .map(|(k, v)| (CompactString::from(*k), v.clone()))
                .collect(),
        )
    };
    let equal_pairs = [
        (int(1), float(1.0)),
        (int(0), float(-0.0)),
        (Value::Null, Value::Null),
        (float(f64::NAN), float(f64::NAN)),
        (
            Value::List(vec![int(1), Value::Null]),
            Value::List(vec![float(1.0), Value::Null]),
        ),
        (
            map(&[("a", int(1)), ("b", string("x"))]),
            map(&[("b", string("x")), ("a", float(1.0))]),
        ),
        (Value::Element(El::V(1)), Value::Element(El::V(1))),
        (
            Value::Path(vec![El::V(1), El::E(2), El::V(3)]),
            Value::Path(vec![El::V(1), El::E(2), El::V(3)]),
        ),
    ];
    for (a, b) in equal_pairs {
        assert!(key(a.clone()) == key(b.clone()), "{a:?} = {b:?}");
        assert_eq!(
            distinct(vec![a.clone(), b.clone()]),
            1,
            "{a:?} {b:?} hash apart"
        );
    }
    let different_pairs = [
        (int(1), string("1")),
        (int(1), float(1.5)),
        (Value::Null, int(0)),
        (Value::Element(El::V(1)), Value::Element(El::E(1))),
        (Value::List(vec![int(1)]), Value::List(vec![int(1), int(1)])),
        (map(&[("a", int(1))]), map(&[("b", int(1))])),
        (
            Value::Path(vec![El::V(1)]),
            Value::List(vec![Value::Element(El::V(1))]),
        ),
        // An integer equals a float only when the float is exactly it (transitive).
        (int((1 << 53) + 1), float((1u64 << 53) as f64)),
        (float(f64::INFINITY), float(f64::NEG_INFINITY)),
        (float(9.3e18), int(i64::MAX)),
    ];
    for (a, b) in different_pairs {
        assert!(key(a.clone()) != key(b.clone()), "{a:?} <> {b:?}");
    }
    // A key of several columns: a vector of keys.
    let rows: HashSet<Vec<GroupKey<El>>> = [
        vec![key(int(1)), key(Value::Null)],
        vec![key(float(1.0)), key(Value::Null)],
        vec![key(int(1)), key(int(0))],
    ]
    .into_iter()
    .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(key(int(4)).into_value(), int(4));
}

// ----- paths -----

#[test]
fn path_values() {
    let compiled = match compile_aggregating::<El>(
        &items("path_length(p), length(p), p = p, path_length(x), p.name"),
        &Names,
    ) {
        Ok(compiled) => compiled,
        Err(e) => panic!("{e:?}"),
    };
    let results: Vec<Result<Value<El>, &'static str>> = compiled
        .outputs
        .iter()
        .map(|output| output.eval(&Row).map_err(|e| e.gqlstatus))
        .collect();
    assert_eq!(
        results,
        [
            Ok(int(1)),
            Ok(int(1)),
            Ok(Truth::True.into_value()),
            Ok(Value::Null),
            Err("22G03"),
        ]
    );
    let wrong = compile_aggregating::<El>(&items("path_length(n)"), &Names)
        .and_then(|c| c.outputs[0].eval(&Row));
    match wrong {
        Err(e) => assert_eq!(e.message, "path_length() takes a path, not an element"),
        Ok(v) => panic!("{v:?}"),
    }
    // A zero-length path has no edge.
    let zero: Value<El> = Value::Path(vec![El::V(1)]);
    assert_eq!(crate::gql::eval::equals(&zero, &zero), Truth::True);
    assert_eq!(
        crate::gql::eval::equals(&zero, &Value::Path(vec![El::V(2)])),
        Truth::False
    );
    // min / max: a path sorts after lists, before strings.
    let rows = [
        one(string("a")),
        one(Value::Path(vec![El::V(1)])),
        one(Value::List(vec![])),
    ];
    assert_eq!(value(Aggregate::Max, false, &rows[..2]), string("a"));
    assert_eq!(
        value(Aggregate::Min, false, &rows[..2]),
        Value::Path(vec![El::V(1)])
    );
    assert_eq!(value(Aggregate::Min, false, &rows), Value::List(vec![]));
    assert_eq!(
        Value::Path(vec![El::V(1), El::E(2), El::V(3)]).into_property(&mut |el: El| {
            ElementProperty::from(match el {
                El::V(id) | El::E(id) => i64::from(id),
            })
        }),
        ElementProperty::from(vec![1i64, 2, 3])
    );
}
