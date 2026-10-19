//! GQL aggregation, the pure part (stage 1b): compiling the items of an aggregating `RETURN` /
//! `WITH`, the accumulators of the aggregate functions, and the GQL grouping equality. The
//! internal step `gql:aggregate` runs them; nothing here reads the traversal pipeline.
//!
//! # Compiling
//!
//! [`compile_aggregating`] compiles the output expressions of an aggregating projection. Every
//! aggregate call inside them (`count(b) + 1` holds one) becomes a read of its result
//! ([`Env::aggregate`]) and an [`AggregateCall`] of its own, whose argument is compiled apart and
//! evaluated per input row. An aggregate inside an aggregate's argument is error 42002.
//!
//! # Accumulating
//!
//! An [`Accumulator`] takes one row at a time with its bulk (a traverser standing for `bulk`
//! equal rows), so no row is ever expanded:
//!
//! - **`NULL` is skipped** by every aggregate except `count(*)`, which counts rows.
//! - **`count`** adds the bulk; the result is an integer (above `i64::MAX`: 22003).
//! - **`sum`** weighs each value by the bulk. Integers stay an integer, summed exactly (in
//!   `i128`) and checked once at the end, so a result that fits is never an error however the
//!   rows arrive (does not fit `i64`: 22003); a float among the inputs makes the result a float.
//!   A non-finite float result from finite inputs is 22003; a non-number is 22G03.
//! - **`avg`** is always a float: the bulk-weighted sum over the number of values; non-numbers
//!   22G03.
//! - **`min` / `max`** order by the total `ORDER BY` order across kinds
//!   ([`compare_values`], C5: never a "cannot compare" error); the bulk does not matter.
//! - **`collect_list`** keeps the values in arrival order, a value of bulk `n` `n` times (stored
//!   once with its bulk until [`Accumulator::finish`]; [`Accumulator::collected`] tells the step
//!   the size first, so it can check its limits before the list is built).
//! - **`DISTINCT`** keeps the first arrival of each value under the grouping equality and ignores
//!   the bulk.
//! - **Empty input** (C4, Neo4j's values): `count` 0, `sum` 0, `collect_list` `[]`, `avg` / `min`
//!   / `max` `NULL`.
//!
//! # Grouping equality
//!
//! [`GroupKey`] wraps a value with the GQL equality of grouping and `DISTINCT` (C3): `NULL`
//! equals `NULL`, numbers are equal when they are the same number (`1` and `1.0` are one value,
//! `-0.0` is `0`; every `NaN` is one value; an integer equals a float only when the float is
//! exactly that integer, so equality stays transitive), lists and paths element-wise, maps by
//! their entries in any order, elements when they are the same element, other scalars by value.
//! Its `Hash` agrees with that equality, so a `Vec<GroupKey<E>>` of the key columns is the hash
//! key of a group without building any text.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use fnv::{FnvBuildHasher, FnvHasher};
use indexmap::IndexSet;

use super::ast::{CallArgs, Expr};
use super::eval::{AggregateRef, Compiled, Env, EvalError, Scope, Value, compile, status};
use super::lexer::Span;
use super::parser::Function;
use super::runtime::compare_values;
use crate::error::GqlHelpTopic;
use crate::value::number::{N, Number};
use crate::{CompactString, ElementKind, ElementProperty};

#[cfg(test)]
mod tests;

/// One aggregate call of an aggregating projection: its function and its compiled argument.
#[derive(Debug, Clone)]
pub(crate) struct AggregateCall<E> {
    /// What the call computes.
    pub(crate) aggregate: Aggregate,
    /// `DISTINCT` before the argument.
    pub(crate) distinct: bool,
    /// The argument, evaluated per input row; `None` for `count(*)`.
    pub(crate) argument: Option<Compiled<E>>,
    /// The whole call (`sum(n.age)`).
    pub(crate) span: Span,
}

impl<E: Clone + Eq + Hash> AggregateCall<E> {
    /// A fresh accumulator of this call (one per group).
    pub(crate) fn accumulator(&self) -> Accumulator<E> {
        Accumulator::new(self.aggregate, self.distinct, self.span)
    }

    /// Adds the row of `env` (standing for `bulk` equal rows) to `accumulator`.
    pub(crate) fn add<En: Env<Element = E>>(
        &self,
        accumulator: &mut Accumulator<E>,
        env: &En,
        bulk: u64,
    ) -> Result<(), EvalError> {
        match &self.argument {
            None => accumulator.add_rows(bulk),
            Some(argument) => accumulator.add(argument.eval(env)?, bulk, env),
        }
    }
}

/// The compiled items of an aggregating projection.
#[derive(Debug, Clone)]
pub(crate) struct Aggregating<E> {
    /// The output expressions, in the order given; an aggregate inside reads its result.
    pub(crate) outputs: Vec<Compiled<E>>,
    /// The aggregate calls; index `i` is what [`Env::aggregate`]`(i)` returns.
    pub(crate) calls: Vec<AggregateCall<E>>,
}

/// Compiles the output expressions of an aggregating `RETURN` / `WITH` (see the module docs).
pub(crate) fn compile_aggregating<'a, E: Clone>(
    exprs: impl IntoIterator<Item = &'a Expr>,
    scope: &impl Scope,
) -> Result<Aggregating<E>, EvalError> {
    let collecting = Collecting {
        inner: scope,
        pending: std::cell::RefCell::new(Vec::new()),
    };
    let outputs = exprs
        .into_iter()
        .map(|expr| compile(expr, &collecting))
        .collect::<Result<Vec<_>, _>>()?;
    let nested = Nested { inner: scope };
    let calls = collecting
        .pending
        .into_inner()
        .into_iter()
        .map(|pending| {
            Ok(AggregateCall {
                aggregate: pending.aggregate,
                distinct: pending.distinct,
                argument: pending
                    .argument
                    .map(|argument| compile(&argument, &nested))
                    .transpose()?,
                span: pending.span,
            })
        })
        .collect::<Result<Vec<_>, EvalError>>()?;
    Ok(Aggregating { outputs, calls })
}

/// An aggregate call met while compiling the outputs, its argument not compiled yet.
struct Pending {
    aggregate: Aggregate,
    distinct: bool,
    argument: Option<Expr>,
    span: Span,
}

/// The scope of the output expressions: records each aggregate call.
struct Collecting<'s, Sc> {
    inner: &'s Sc,
    pending: std::cell::RefCell<Vec<Pending>>,
}

impl<Sc: Scope> Scope for Collecting<'_, Sc> {
    fn variable(&self, name: &str) -> Option<usize> {
        self.inner.variable(name)
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        self.inner.parameter(name)
    }

    fn aggregate(&self, call: AggregateRef<'_>) -> Option<Result<usize, EvalError>> {
        let argument = match call.args {
            CallArgs::Star(_) => None,
            // The parser and `compile` admit exactly one argument.
            CallArgs::Exprs(exprs) => exprs.first().cloned(),
        };
        let Some(aggregate) = Aggregate::of(call.function, argument.is_some()) else {
            return Some(Err(EvalError::new(
                status::SEMANTIC,
                "not an aggregate function".to_string(),
                call.span,
            )));
        };
        let mut pending = self.pending.borrow_mut();
        let index = pending.len();
        pending.push(Pending {
            aggregate,
            distinct: call.distinct,
            argument,
            span: call.span,
        });
        Some(Ok(index))
    }
}

/// The scope of an aggregate's argument: no aggregate inside.
struct Nested<'s, Sc> {
    inner: &'s Sc,
}

impl<Sc: Scope> Scope for Nested<'_, Sc> {
    fn variable(&self, name: &str) -> Option<usize> {
        self.inner.variable(name)
    }

    fn parameter(&self, name: &str) -> Option<ElementProperty> {
        self.inner.parameter(name)
    }

    fn aggregate(&self, call: AggregateRef<'_>) -> Option<Result<usize, EvalError>> {
        Some(Err(EvalError::new(
            status::SEMANTIC,
            "an aggregate function cannot be used inside the argument of another \
                      aggregate"
                .to_string(),
            call.span,
        )))
    }
}

/// An [`Env`] that answers [`Env::aggregate`] from the results of a group and everything else
/// from `env` (the row the group's key variables are read from).
pub(crate) struct WithAggregates<'a, En: Env> {
    /// The row of the group.
    pub(crate) env: &'a En,
    /// The finished aggregate results, by call index.
    pub(crate) results: &'a [Value<En::Element>],
}

impl<En: Env> Env for WithAggregates<'_, En> {
    type Element = En::Element;

    fn variable(&self, slot: usize) -> Value<Self::Element> {
        self.env.variable(slot)
    }

    fn kind(&self, element: &Self::Element) -> ElementKind {
        self.env.kind(element)
    }

    fn element_id(&self, element: &Self::Element) -> CompactString {
        self.env.element_id(element)
    }

    fn property(&self, element: &Self::Element, key: &str) -> Option<ElementProperty> {
        self.env.property(element, key)
    }

    fn has_label(&self, element: &Self::Element, label: &str) -> bool {
        self.env.has_label(element, label)
    }

    fn has_any_label(&self, element: &Self::Element) -> bool {
        self.env.has_any_label(element)
    }

    fn labels(&self, element: &Self::Element) -> Vec<CompactString> {
        self.env.labels(element)
    }

    fn aggregate(&self, index: usize) -> Value<Self::Element> {
        self.results.get(index).cloned().unwrap_or(Value::Null)
    }
}

/// What an [`Accumulator`] computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Aggregate {
    /// `count(*)`: rows, `NULL` or not.
    CountRows,
    /// `count(x)`: non-`NULL` values.
    Count,
    /// `sum(x)`.
    Sum,
    /// `avg(x)`.
    Avg,
    /// `min(x)`.
    Min,
    /// `max(x)`.
    Max,
    /// `collect_list(x)`, Cypher `collect(x)`.
    CollectList,
}

impl Aggregate {
    /// The aggregate of `function`; `argument`: not `count(*)`. `None` for a scalar function.
    pub(crate) fn of(function: Function, argument: bool) -> Option<Self> {
        Some(match function {
            Function::Count if argument => Aggregate::Count,
            Function::Count => Aggregate::CountRows,
            Function::Sum => Aggregate::Sum,
            Function::Avg => Aggregate::Avg,
            Function::Min => Aggregate::Min,
            Function::Max => Aggregate::Max,
            Function::CollectList => Aggregate::CollectList,
            Function::ElementId
            | Function::Labels
            | Function::Type
            | Function::Coalesce
            | Function::Upper
            | Function::Lower
            | Function::CharLength
            | Function::Size
            | Function::Cardinality
            | Function::PathLength => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Aggregate::CountRows | Aggregate::Count => "count",
            Aggregate::Sum => "sum",
            Aggregate::Avg => "avg",
            Aggregate::Min => "min",
            Aggregate::Max => "max",
            Aggregate::CollectList => "collect_list",
        }
    }
}

/// The running sum of numbers: integers exactly, floats apart.
#[derive(Debug, Clone, Default)]
struct Numeric {
    integers: i128,
    floats: f64,
    /// A float was added.
    any_float: bool,
    /// A non-finite float was added (then a non-finite result is no overflow).
    any_non_finite: bool,
    /// How many values (bulk included).
    count: u128,
}

impl Numeric {
    fn add(&mut self, number: &Number, bulk: u64, span: Span) -> Result<(), EvalError> {
        match &number.n {
            N::NegInt(value) => {
                // `i64 * u64` fits `i128`; only the running total can leave it.
                let weighted = i128::from(*value) * i128::from(bulk);
                self.integers = self
                    .integers
                    .checked_add(weighted)
                    .ok_or_else(|| out_of_range(span))?;
            }
            N::Float(value) => {
                self.any_float = true;
                self.any_non_finite |= !value.0.is_finite();
                self.floats += value.0 * bulk as f64;
            }
        }
        self.count = self.count.saturating_add(u128::from(bulk));
        Ok(())
    }

    fn total(&self) -> f64 {
        self.integers as f64 + self.floats
    }

    fn sum<E>(&self, span: Span) -> Result<Value<E>, EvalError> {
        if self.any_float {
            return finite(self.total(), self.any_non_finite, span);
        }
        i64::try_from(self.integers)
            .map(integer)
            .map_err(|_| out_of_range(span))
    }

    fn avg<E>(&self, span: Span) -> Result<Value<E>, EvalError> {
        if self.count == 0 {
            return Ok(Value::Null);
        }
        let average = if self.any_float {
            self.total() / self.count as f64
        } else {
            // Exact integer part first, so a sum beyond f64's exact range keeps its precision.
            let count = self.count as i128;
            let whole = self.integers / count;
            let rest = self.integers % count;
            whole as f64 + rest as f64 / count as f64
        };
        finite(average, self.any_non_finite, span)
    }
}

fn finite<E>(value: f64, any_non_finite: bool, span: Span) -> Result<Value<E>, EvalError> {
    if !value.is_finite() && !any_non_finite {
        return Err(out_of_range(span));
    }
    Ok(Value::Scalar(ElementProperty::Number(Number::from_f64(
        value,
    ))))
}

fn integer<E>(value: i64) -> Value<E> {
    Value::Scalar(ElementProperty::Number(Number::from_i64(value)))
}

fn out_of_range(span: Span) -> EvalError {
    EvalError::new(
        status::OUT_OF_RANGE,
        "numeric value out of range: the aggregate does not fit".to_string(),
        span,
    )
}

#[derive(Debug, Clone)]
enum State<E> {
    Count(u128),
    Numeric(Numeric),
    Extreme(Option<Value<E>>),
    /// Values with their bulk, in arrival order (`DISTINCT`: kept in the seen set instead).
    Collect(Vec<(Value<E>, u64)>),
}

/// The running state of one aggregate call for one group (see the module docs).
#[derive(Debug, Clone)]
pub(crate) struct Accumulator<E> {
    function: Aggregate,
    state: State<E>,
    /// `DISTINCT`: the values seen so far, in arrival order (not for `min` / `max`, where
    /// duplicates change nothing).
    seen: Option<IndexSet<GroupKey<E>, FnvBuildHasher>>,
    span: Span,
}

impl<E: Clone + Eq + Hash> Accumulator<E> {
    /// An empty accumulator; `span` is the call, for errors.
    pub(crate) fn new(function: Aggregate, distinct: bool, span: Span) -> Self {
        let state = match function {
            Aggregate::CountRows | Aggregate::Count => State::Count(0),
            Aggregate::Sum | Aggregate::Avg => State::Numeric(Numeric::default()),
            Aggregate::Min | Aggregate::Max => State::Extreme(None),
            Aggregate::CollectList => State::Collect(Vec::new()),
        };
        let seen = (distinct
            && !matches!(
                function,
                Aggregate::Min | Aggregate::Max | Aggregate::CountRows
            ))
        .then(IndexSet::default);
        Accumulator {
            function,
            state,
            seen,
            span,
        }
    }

    /// Adds `bulk` rows to `count(*)` (other aggregates ignore it).
    pub(crate) fn add_rows(&mut self, bulk: u64) -> Result<(), EvalError> {
        if let (Aggregate::CountRows, State::Count(count)) = (self.function, &mut self.state) {
            *count = count.saturating_add(u128::from(bulk));
        }
        Ok(())
    }

    /// Adds `value`, standing for `bulk` equal values; `env` resolves element ids for `min` /
    /// `max`.
    pub(crate) fn add<En: Env<Element = E>>(
        &mut self,
        value: Value<E>,
        bulk: u64,
        env: &En,
    ) -> Result<(), EvalError> {
        if self.function == Aggregate::CountRows {
            return self.add_rows(bulk);
        }
        if value.is_null() || bulk == 0 {
            return Ok(());
        }
        let Some(seen) = &mut self.seen else {
            return self.accept(value, bulk, env);
        };
        let key = GroupKey(value);
        if seen.contains(&key) {
            return Ok(());
        }
        match self.function {
            // The seen set is the list itself.
            Aggregate::CollectList => {
                seen.insert(key);
                Ok(())
            }
            _ => {
                accept(self.function, &mut self.state, &key.0, 1, self.span)?;
                seen.insert(key);
                Ok(())
            }
        }
    }

    fn accept<En: Env<Element = E>>(
        &mut self,
        value: Value<E>,
        bulk: u64,
        env: &En,
    ) -> Result<(), EvalError> {
        match &mut self.state {
            State::Collect(items) => {
                // A run of equal values arriving one after another could merge; the step's
                // bulk merging already does that upstream.
                items.push((value, bulk));
                Ok(())
            }
            State::Extreme(best) => {
                let replace = match best {
                    None => true,
                    Some(current) => {
                        let order = compare_values(&value, current, env);
                        match self.function {
                            Aggregate::Min => order == Ordering::Less,
                            _ => order == Ordering::Greater,
                        }
                    }
                };
                if replace {
                    *best = Some(value);
                }
                Ok(())
            }
            state => accept(self.function, state, &value, bulk, self.span),
        }
    }

    /// How many items `collect_list` will hold (bulk included); 0 for the other aggregates.
    /// The step checks its limits with it before [`Self::finish`] builds the list.
    pub(crate) fn collected(&self) -> u64 {
        match (&self.state, &self.seen) {
            (State::Collect(_), Some(seen)) => seen.len() as u64,
            (State::Collect(items), None) => items
                .iter()
                .fold(0u64, |total, (_, bulk)| total.saturating_add(*bulk)),
            _ => 0,
        }
    }

    /// The result (see the module docs for empty input).
    pub(crate) fn finish(self) -> Result<Value<E>, EvalError> {
        let span = self.span;
        match self.state {
            State::Count(count) => i64::try_from(count)
                .map(integer)
                .map_err(|_| out_of_range(span)),
            State::Numeric(numeric) if self.function == Aggregate::Avg => numeric.avg(span),
            State::Numeric(numeric) => numeric.sum(span),
            State::Extreme(best) => Ok(best.unwrap_or(Value::Null)),
            State::Collect(items) => match self.seen {
                Some(seen) => Ok(Value::List(seen.into_iter().map(|key| key.0).collect())),
                None => collect(items, span),
            },
        }
    }
}

/// Adds a value to a count or a sum.
fn accept<E>(
    function: Aggregate,
    state: &mut State<E>,
    value: &Value<E>,
    bulk: u64,
    span: Span,
) -> Result<(), EvalError> {
    match state {
        State::Count(count) => {
            *count = count.saturating_add(u128::from(bulk));
            Ok(())
        }
        State::Numeric(numeric) => match value {
            Value::Scalar(ElementProperty::Number(number)) => numeric.add(number, bulk, span),
            other => Err(EvalError::new(
                status::INVALID_VALUE_TYPE,
                format!(
                    "{}() takes numbers, not {}",
                    function.name(),
                    kind_name(other)
                ),
                span,
            )
            .with_topic(GqlHelpTopic::AggregateValue)),
        },
        // `min` / `max` and `collect_list` are handled by `Accumulator::accept`; with
        // `DISTINCT` only `collect_list` keeps its values in the seen set.
        State::Extreme(_) | State::Collect(_) => Ok(()),
    }
}

/// The collected list, each value repeated by its bulk.
fn collect<E: Clone>(items: Vec<(Value<E>, u64)>, span: Span) -> Result<Value<E>, EvalError> {
    let total = items
        .iter()
        .fold(0u64, |total, (_, bulk)| total.saturating_add(*bulk));
    let too_large = || {
        EvalError::new(
            status::OUT_OF_RANGE,
            "collect_list() would hold more items than fit in memory".to_string(),
            span,
        )
    };
    let capacity = usize::try_from(total).map_err(|_| too_large())?;
    let mut list = Vec::new();
    list.try_reserve_exact(capacity).map_err(|_| too_large())?;
    for (value, bulk) in items {
        for _ in 1..bulk {
            list.push(value.clone());
        }
        list.push(value);
    }
    Ok(Value::List(list))
}

fn kind_name<E>(value: &Value<E>) -> &'static str {
    match value {
        Value::Null => "NULL",
        Value::Scalar(ElementProperty::String(_)) => "a string",
        Value::Scalar(ElementProperty::Boolean(_)) => "a boolean",
        Value::Scalar(_) => "a non-numeric scalar value",
        Value::List(_) => "a list",
        Value::Map(_) => "a map",
        Value::Element(_) => "an element",
        Value::Path(_) => "a path",
    }
}

/// A value under the GQL grouping equality (see the module docs), with a hash that agrees.
#[derive(Debug, Clone)]
pub(crate) struct GroupKey<E>(pub(crate) Value<E>);

impl<E> GroupKey<E> {
    /// The value.
    #[cfg_attr(not(test), allow(dead_code, reason = "used by the tests"))]
    pub(crate) fn into_value(self) -> Value<E> {
        self.0
    }
}

impl<E: Eq> PartialEq for GroupKey<E> {
    fn eq(&self, other: &Self) -> bool {
        group_eq(&self.0, &other.0)
    }
}

impl<E: Eq> Eq for GroupKey<E> {}

impl<E: Hash> Hash for GroupKey<E> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        group_hash(&self.0, state);
    }
}

/// A number as the grouping sees it.
#[derive(PartialEq, Eq, Hash)]
enum NumberKey {
    /// An integer, or a float that is exactly one.
    Integer(i64),
    /// Any other float, by its bits.
    Float(u64),
    /// Every `NaN`.
    NaN,
}

fn number_key(number: &Number) -> NumberKey {
    match &number.n {
        N::NegInt(value) => NumberKey::Integer(*value),
        N::Float(value) => {
            let value = value.0;
            // `-2^63` is an `i64`, `2^63` is not; `fract()` of an infinity is `NaN`.
            if value.is_nan() {
                NumberKey::NaN
            } else if value.fract() == 0.0
                && (-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&value)
            {
                NumberKey::Integer(value as i64)
            } else {
                NumberKey::Float(value.to_bits())
            }
        }
    }
}

fn group_eq<E: Eq>(a: &Value<E>, b: &Value<E>) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Scalar(ElementProperty::Number(a)), Value::Scalar(ElementProperty::Number(b))) => {
            number_key(a) == number_key(b)
        }
        (Value::Scalar(a), Value::Scalar(b)) => a == b,
        (Value::List(a), Value::List(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| group_eq(a, b))
        }
        (Value::Map(a), Value::Map(b)) => {
            // Keys are unique in a map.
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.iter()
                        .find(|(other, _)| other == key)
                        .is_some_and(|(_, other)| group_eq(value, other))
                })
        }
        (Value::Element(a), Value::Element(b)) => a == b,
        (Value::Path(a), Value::Path(b)) => a == b,
        _ => false,
    }
}

fn group_hash<E: Hash, H: Hasher>(value: &Value<E>, state: &mut H) {
    match value {
        Value::Null => state.write_u8(0),
        Value::Scalar(ElementProperty::Number(number)) => {
            state.write_u8(1);
            number_key(number).hash(state);
        }
        Value::Scalar(scalar) => {
            state.write_u8(2);
            scalar.hash(state);
        }
        Value::List(items) => {
            state.write_u8(3);
            state.write_usize(items.len());
            for item in items {
                group_hash(item, state);
            }
        }
        Value::Map(entries) => {
            // Equality ignores the entry order, so the hash does too.
            state.write_u8(4);
            state.write_usize(entries.len());
            let mut sum = 0u64;
            for (key, value) in entries {
                let mut entry = FnvHasher::default();
                key.hash(&mut entry);
                group_hash(value, &mut entry);
                sum = sum.wrapping_add(entry.finish());
            }
            state.write_u64(sum);
        }
        Value::Element(element) => {
            state.write_u8(5);
            element.hash(state);
        }
        Value::Path(elements) => {
            state.write_u8(6);
            elements.hash(state);
        }
    }
}
