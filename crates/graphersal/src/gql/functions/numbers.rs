//! Numeric functions (stage 1c task T4): `abs`, `floor`, `ceil` / `ceiling`, `round`, `sign`, `sqrt`,
//! `exp`, `ln`, `log` (one argument: natural; two: GQL `LOG(base, x)`), `log10`, `power`, `mod`.
//!
//! The contract of a family file is in the module docs of `functions/mod.rs`; unit tests use
//! the test `Env` and helpers of `functions/test_env.rs`.

use super::super::eval::{Env, EvalError, Value};
use super::super::lexer::Span;
use super::super::parser::Function;
use super::{Arg, function_not_implemented};

/// Evaluates the numeric function `function` (its family, [`super::Family::Numbers`]) on its
/// evaluated arguments.
pub(crate) fn call<En: Env>(
    function: Function,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let _ = (args, env);
    Err(function_not_implemented(function, span))
}
