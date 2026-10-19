//! Conversions (design K-D22, F10; stage 1c task T2): GQL `CAST(x AS INTEGER | FLOAT | STRING |
//! BOOLEAN)` (strict), Cypher `toInteger` / `toFloat` / `toBoolean` / `toString` (lenient) and
//! their `...OrNull` forms, one core with a [`Mode`].
//!
//! The contract of a family file is in the module docs of `functions/mod.rs`; unit tests use
//! the test `Env` and helpers of `functions/test_env.rs`.

use super::super::eval::{Env, EvalError, Value};
use super::super::lexer::Span;
use super::{Arg, Conversion, Mode, not_implemented};

/// Converts `arg` as `conversion` says (the target and the [`Mode`]); `span` is the whole call.
pub(crate) fn convert<En: Env>(
    conversion: Conversion,
    arg: Arg<En::Element>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let _ = (arg, env, Mode::Cast);
    Err(not_implemented(conversion.name(), span))
}
