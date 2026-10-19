//! Lists, elements and paths (stage 1c task T5): `head`, `last`, `tail`, GQL's list `trim(list, n)`,
//! `range`, `keys`, `properties`, `startNode`, `endNode`, `nodes`, `relationships`, and the GQL
//! predicates `IS [NOT] SOURCE OF` / `IS [NOT] DESTINATION OF`.
//!
//! The contract of a family file is in the module docs of `functions/mod.rs`; unit tests use
//! the test `Env` and helpers of `functions/test_env.rs`.

use super::super::ast::Endpoint;
use super::super::eval::{Env, EvalError, Value};
use super::super::lexer::Span;
use super::super::parser::Function;
use super::{Arg, function_not_implemented, not_implemented};

/// Evaluates the list, element or path function `function` (its family,
/// [`super::Family::Lists`], and `trim` with a list first argument) on its evaluated arguments.
pub(crate) fn call<En: Env>(
    function: Function,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let _ = (args, env);
    Err(function_not_implemented(function, span))
}

/// `operand IS [NOT] SOURCE OF edge` / `operand IS [NOT] DESTINATION OF edge`; `span` is the
/// whole predicate.
pub(crate) fn is_endpoint<En: Env>(
    operand: &Value<En::Element>,
    edge: &Value<En::Element>,
    end: Endpoint,
    negated: bool,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let _ = (operand, edge, end, negated, env);
    Err(not_implemented("IS SOURCE OF / IS DESTINATION OF", span))
}
