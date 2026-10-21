//! Arithmetic with temporal operands (3g.T6), Neo4j's rules (`DurationValue.addTo` /
//! `subtractFrom`, `mul`, `div`) on the engine's values:
//!
//! | Form | Result |
//! |---|---|
//! | `x + dur`, `dur + x`, `x - dur` (`x` a `date`, `localtime`, `localdatetime` or `datetime`) | a value of `x`'s type: the duration's months, then its days, then its time part, each where the type has it (U5) |
//! | `dur + dur`, `dur - dur` | the part-wise sum / difference (months, days, time each on its own: `PT49H` stays) |
//! | `dur * n`, `n * dur`, `dur / n` | each part scaled, then the fraction cascade of `duration({..})` (`P1M * 0.5` = `P15DT5H14M33S`, U11) |
//! | `-dur`, `+dur` | every part negated / the duration itself |
//!
//! U5, the parts a type lacks: a `date` takes the months, the days and the **whole days** of the
//! time part (truncated toward zero, as Java's `long` division: `PT33H` adds one day, `PT-33H`
//! takes one away), and ignores the rest; a `localtime` takes only the time part and wraps
//! around midnight. A `localdatetime` / `datetime` takes all three (a `datetime` keeps its
//! offset: the months and days move its wall clock, the time part its instant).
//! `x - dur` is `x + (-dur)`, which is Java's `minus` for each of the three steps. The Gremlin
//! `date_add` keeps its own rules (a time part on a date is an error there).
//!
//! Errors: two values that are not both durations cannot be added or subtracted (22G03, the
//! advice names `duration.between(a, b)` for the time between two values), a number with a
//! temporal value only multiplies or divides a duration (22G03), a zero divisor is 22012 before
//! anything else, a result outside the years -9999 to 9999 or the duration fields 22008, a
//! factor that is no finite number below 2^63 22003, and a scaled duration with a part below
//! the microsecond 0A000 (U1: values hold microseconds; never cut silently). Each 22G03 error
//! carries [`GqlHelpTopic::TemporalArithmetic`]. The evaluator calls [`binary`] for `+`, `-`,
//! `*` and `/` when an operand is a temporal value (neither `NULL`), and [`sign`] for a unary
//! `-` / `+` on one.

use super::super::super::ast::{BinaryOp, UnaryOp};
use super::super::super::eval::{EvalError, Value};
use super::super::super::lexer::Span;
use super::construct::temporal_error;
use super::sub_micro;
use crate::ElementProperty;
use crate::error::GqlHelpTopic;
use crate::value::number::{N, Number};
use crate::value::temporal::arith::{self, Amount};
use crate::value::temporal::civil::MICROS_PER_DAY;
use crate::value::temporal::{Duration, Temporal, TemporalError};

/// `left op right` with at least one temporal operand, none `NULL`.
pub(crate) fn binary<E>(
    op: BinaryOp,
    left: &Value<E>,
    right: &Value<E>,
    span: Span,
) -> Result<Value<E>, EvalError> {
    let (a, b) = (scalar(left), scalar(right));
    let result = match (op, a, b) {
        (BinaryOp::Add, Some(Operand::Temporal(l)), Some(Operand::Temporal(r)))
        | (BinaryOp::Sub, Some(Operand::Temporal(l)), Some(Operand::Temporal(r))) => {
            let negate = op == BinaryOp::Sub;
            match (l, r) {
                (_, Temporal::Duration(d)) => add(l, d, negate, span)?,
                // `dur + x` is `x + dur`; `dur - x` has no meaning.
                (Temporal::Duration(d), _) if !negate => add(r, d, false, span)?,
                _ => return Err(not_both_durations(op, left, right, span)),
            }
        }
        (
            BinaryOp::Mul,
            Some(Operand::Temporal(Temporal::Duration(d))),
            Some(Operand::Number(n)),
        )
        | (
            BinaryOp::Mul,
            Some(Operand::Number(n)),
            Some(Operand::Temporal(Temporal::Duration(d))),
        ) => scale(d, n, false, span)?,
        (
            BinaryOp::Div,
            Some(Operand::Temporal(Temporal::Duration(d))),
            Some(Operand::Number(n)),
        ) => scale(d, n, true, span)?,
        _ => return Err(wrong_operands(op, left, right, span)),
    };
    Ok(Value::Scalar(ElementProperty::Temporal(result)))
}

/// `-value` / `+value` of a temporal value: only a duration has a sign.
pub(crate) fn sign<E>(op: UnaryOp, value: &Temporal, span: Span) -> Result<Value<E>, EvalError> {
    match (op, value) {
        (UnaryOp::Plus, Temporal::Duration(_)) => {
            Ok(Value::Scalar(ElementProperty::Temporal(*value)))
        }
        (UnaryOp::Minus, Temporal::Duration(d)) => d
            .checked_neg()
            .map(|d| Value::Scalar(ElementProperty::Temporal(Temporal::Duration(d))))
            .map_err(|e| temporal_error(e, span)),
        (UnaryOp::Minus | UnaryOp::Plus | UnaryOp::Not, _) => Err(EvalError::invalid_type(
            format!(
                "a sign applies to a number or a duration, not {}",
                super::kind_name(value)
            ),
            span,
        )
        .with_topic(GqlHelpTopic::TemporalArithmetic)),
    }
}

/// A non-`NULL` scalar operand the temporal arithmetic reads.
#[derive(Clone, Copy)]
enum Operand<'a> {
    Temporal(Temporal),
    Number(&'a Number),
}

fn scalar<E>(value: &Value<E>) -> Option<Operand<'_>> {
    match value {
        Value::Scalar(ElementProperty::Temporal(t)) => Some(Operand::Temporal(*t)),
        Value::Scalar(ElementProperty::Number(n)) => Some(Operand::Number(n)),
        _ => None,
    }
}

/// `x + d` (`negate` false) or `x - d` (`negate` true) with Neo4j's rules for the parts `x`'s
/// type lacks (U5, module docs).
fn add(x: Temporal, d: Duration, negate: bool, span: Span) -> Result<Temporal, EvalError> {
    let error = |e: TemporalError| temporal_error(e, span);
    let d = if negate {
        d.checked_neg().map_err(error)?
    } else {
        d
    };
    let d = match x {
        Temporal::Date(_) => {
            // The whole days of the time part (truncated toward zero), nothing below.
            let extra = d.micros() / MICROS_PER_DAY;
            let days = i32::try_from(d.days() as i64 + extra)
                .map_err(|_| error(TemporalError::out_of_range("the duration")))?;
            Duration::new(d.months(), days, 0)
        }
        Temporal::Time(_) => Duration::new(0, 0, d.micros()),
        Temporal::LocalDateTime(_) | Temporal::DateTime(_) | Temporal::Duration(_) => d,
    };
    arith::add_duration(x, d, None).map_err(error)
}

/// `d * n` or `d / n` (`divide`), the parts scaled and cascaded (Neo4j `mul` / `div`).
fn scale(d: Duration, n: &Number, divide: bool, span: Span) -> Result<Temporal, EvalError> {
    let factor = match &n.n {
        N::NegInt(value) => Some(Amount::from_i64(*value)),
        N::Float(value) => Amount::from_f64(value.0),
    };
    let zero = match &n.n {
        N::NegInt(value) => *value == 0,
        N::Float(value) => value.0 == 0.0,
    };
    if divide && zero {
        return Err(EvalError::division_by_zero(span));
    }
    let Some(factor) = factor else {
        return Err(EvalError::out_of_range(span));
    };
    let scaled = arith::scale_duration(d, factor, divide).map_err(|e| temporal_error(e, span))?;
    if scaled.sub_micro {
        return Err(sub_micro(span));
    }
    Ok(Temporal::Duration(scaled.duration))
}

/// The symbol of an arithmetic operator.
fn symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Pow => "^",
        BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Xor
        | BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::Lt
        | BinaryOp::Le
        | BinaryOp::Gt
        | BinaryOp::Ge
        | BinaryOp::In
        | BinaryOp::StartsWith
        | BinaryOp::EndsWith
        | BinaryOp::Contains
        | BinaryOp::Concat => "an operator",
    }
}

/// The 22G03 error of `+` / `-` on two temporal values none of which is a duration (or of
/// `dur - x`).
#[cold]
fn not_both_durations<E>(op: BinaryOp, left: &Value<E>, right: &Value<E>, span: Span) -> EvalError {
    let message = if op == BinaryOp::Sub {
        let advice = if is_duration(left) {
            ""
        } else {
            "; the time between two values is duration.between(a, b)"
        };
        format!(
            "- subtracts a duration from a temporal value or a duration, not {} from {}{advice}",
            right.kind_name(),
            left.kind_name()
        )
    } else {
        format!(
            "+ adds a duration to a temporal value or to a duration, not {} and {}",
            left.kind_name(),
            right.kind_name()
        )
    };
    EvalError::invalid_type(message, span).with_topic(GqlHelpTopic::TemporalArithmetic)
}

/// The 22G03 error of any other operand pair with a temporal value.
#[cold]
fn wrong_operands<E>(op: BinaryOp, left: &Value<E>, right: &Value<E>, span: Span) -> EvalError {
    let what = match op {
        BinaryOp::Add | BinaryOp::Sub => "a temporal value and a duration",
        BinaryOp::Mul => "a duration and a number",
        BinaryOp::Div => "a duration (divided by a number)",
        _ => "numbers",
    };
    EvalError::invalid_type(
        format!(
            "{} with a temporal value takes {what}, not {} and {}",
            symbol(op),
            left.kind_name(),
            right.kind_name()
        ),
        span,
    )
    .with_topic(GqlHelpTopic::TemporalArithmetic)
}

fn is_duration<E>(value: &Value<E>) -> bool {
    matches!(
        value,
        Value::Scalar(ElementProperty::Temporal(Temporal::Duration(_)))
    )
}
