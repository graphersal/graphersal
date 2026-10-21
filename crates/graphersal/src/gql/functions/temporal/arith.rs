//! Arithmetic with temporal operands (3g.T6): `x + dur`, `x - dur`, `dur + x` for every type
//! (a date takes the whole days of a duration's time part, a time ignores months and days and
//! wraps, U5), `dur ± dur`, `dur * n`, `dur / n` (the fraction cascade of U11), the sign of a
//! duration. The evaluator calls [`binary`] for `+`, `-`, `*` and `/` when an operand is a
//! temporal value (neither `NULL`), and [`sign`] for a unary `-` / `+` on one. Stubs until
//! 3g.T6.

use super::super::super::ast::{BinaryOp, UnaryOp};
use super::super::super::eval::{EvalError, Value};
use super::super::super::lexer::Span;
use super::not_yet;
use crate::value::temporal::Temporal;

/// `left op right` with at least one temporal operand, none `NULL`.
pub(crate) fn binary<E>(
    op: BinaryOp,
    _left: &Value<E>,
    _right: &Value<E>,
    span: Span,
) -> Result<Value<E>, EvalError> {
    let symbol = match op {
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
    };
    Err(not_yet(&format!("{symbol} on temporal values"), span))
}

/// `-value` / `+value` of a temporal value.
pub(crate) fn sign<E>(op: UnaryOp, _value: &Temporal, span: Span) -> Result<Value<E>, EvalError> {
    let symbol = match op {
        UnaryOp::Minus => "-",
        UnaryOp::Plus => "+",
        UnaryOp::Not => "NOT",
    };
    Err(not_yet(
        &format!("the sign {symbol} of a temporal value"),
        span,
    ))
}
