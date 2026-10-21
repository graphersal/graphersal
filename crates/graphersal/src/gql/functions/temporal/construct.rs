//! Constructors and clocks (3g.T4): `date(..)`, `local_time(..)` / `localtime(..)`,
//! `local_datetime(..)` / `localdatetime(..)`, `zoned_datetime(..)` / `datetime(..)` from a map,
//! a string or another temporal value (no argument: the time the query started, the pinned
//! `now()`), `duration(map | string)`, `X.statement([map])` / `X.transaction([map])` (the pinned
//! `now()`, U9) and `datetime.fromepoch(seconds, nanoseconds)` /
//! `datetime.fromepochmillis(milliseconds)`. A sub-microsecond part is 0A000 (U1), never cut.
//! Stubs until 3g.T4.

use super::super::super::eval::{Env, EvalError, Value};
use super::super::super::lexer::Span;
use super::super::super::parser::{Clock, TemporalFunction, TemporalType};
use super::super::Arg;
use super::{cypher_name, not_yet};

fn stub(function: TemporalFunction, span: Span) -> EvalError {
    not_yet(&format!("{}()", cypher_name(function)), span)
}

/// `date(..)`, `localtime(..)`, `localdatetime(..)`, `datetime(..)` (zero or one argument).
pub(crate) fn construct<En: Env>(
    of: TemporalType,
    _args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    Err(stub(TemporalFunction::Construct(of), span))
}

/// `duration(map | string)`.
pub(crate) fn duration<En: Env>(
    _args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    Err(stub(TemporalFunction::Duration, span))
}

/// `X.statement([map])` / `X.transaction([map])`.
pub(crate) fn clock<En: Env>(
    of: TemporalType,
    clock: Clock,
    _args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    Err(stub(TemporalFunction::Clock(of, clock), span))
}

/// `datetime.fromepoch(seconds, nanoseconds)`.
pub(crate) fn from_epoch<En: Env>(
    _args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    Err(stub(TemporalFunction::FromEpoch, span))
}

/// `datetime.fromepochmillis(milliseconds)`.
pub(crate) fn from_epoch_millis<En: Env>(
    _args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    Err(stub(TemporalFunction::FromEpochMillis, span))
}
