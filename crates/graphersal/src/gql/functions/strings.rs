//! String functions (stage 1c task T3): `left`, `right`, `substring`, `trim` (strings and the SQL
//! `TRIM` forms), `btrim` / `ltrim` / `rtrim`, `replace`, `split`, `reverse`, `byte_length` /
//! `octet_length`, `isEmpty`, `nullif`.
//!
//! The contract of a family file is in the module docs of `functions/mod.rs`; unit tests use
//! the test `Env` and helpers of `functions/test_env.rs`.

use super::super::ast::TrimSide;
use super::super::eval::{Env, EvalError, Value};
use super::super::lexer::Span;
use super::super::parser::Function;
use super::{Arg, function_not_implemented};

/// Evaluates the string function `function` (its family, [`super::Family::Strings`]; `trim`
/// with a list first argument goes to `lists.rs`) on its evaluated arguments.
pub(crate) fn call<En: Env>(
    function: Function,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let _ = (args, env);
    Err(function_not_implemented(function, span))
}

/// The SQL forms of `TRIM`: `TRIM([LEADING | TRAILING | BOTH] [character] FROM source)`
/// (`character`: `None` for the default, the space, design F11).
pub(crate) fn trim<En: Env>(
    side: TrimSide,
    character: Option<Arg<En::Element>>,
    source: Arg<En::Element>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let _ = (side, character, source, env);
    Err(function_not_implemented(Function::Trim, span))
}
