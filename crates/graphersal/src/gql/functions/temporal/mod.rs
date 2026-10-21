//! The temporal functions of stage 3g: constructors, clocks, component access, arithmetic,
//! truncation and differences of the five engine temporal types (`crate::value::temporal`), with
//! the openCypher (Neo4j) meanings the user decided in 3g.P0 (U1-U15). The engine module is used
//! as it is; Gremlin / Rhai keep their PostgreSQL-style semantics (`date_trunc`, `date_part`,
//! `age`, ...), and every difference lives here.
//!
//! [`call`] is the entry of [`super::call`] for [`Function::Temporal`](super::Function); the
//! evaluator also routes the property access `x.year` on a temporal value to [`component`] and
//! the arithmetic operators with a temporal operand to [`arith`].
//!
//! | Part | File | Entry | Task |
//! |---|---|---|---|
//! | `date(..)`, `local_time(..)`, `local_datetime(..)`, `zoned_datetime(..)` and the Cypher names, `duration(..)`, `X.statement()` / `X.transaction()`, `datetime.fromepoch(millis)` | `construct.rs` | [`construct::construct`], [`construct::duration`], [`construct::clock`], [`construct::from_epoch`], [`construct::from_epoch_millis`] | 3g.T4 |
//! | components `x.year`, ..., `toString(x)` in the Neo4j text form | `ops.rs` | [`ops::component`], [`ops::to_text`] | 3g.T5 |
//! | `+` / `-` with durations, `dur * n`, `dur / n`, the sign | `arith.rs` | [`arith::binary`], [`arith::sign`] | 3g.T6 |
//! | `X.truncate(unit, value [, map])` | `truncate.rs` | [`truncate::truncate`] | 3g.T7 |
//! | `duration.between`, `duration.inMonths` / `inDays` / `inSeconds` | `between.rs` | [`between::between`] | 3g.T8 |
//!
//! Until a part lands, its entry raises 0A000 "not yet" ([`not_yet`]), so a query using it is
//! refused clearly and never answered wrongly. Cypher's zoned `time()` (U2) and `X.realtime()`
//! (U9) never reach this module: the parser refuses them (`parser::NOT_YET_FUNCTIONS`).
//!
//! The family contract of [`super`] holds: the arity is the parser's, `NULL` propagates (F2),
//! a wrong kind is 22G03, no panics, exhaustive matches.

use super::super::eval::{Env, EvalError, Value, status};
use super::super::lexer::Span;
use super::super::parser::{FUNCTIONS, Function, TemporalFunction};
use super::Arg;
use crate::value::temporal::Temporal;

pub(crate) mod arith;
pub(crate) mod between;
pub(crate) mod construct;
pub(crate) mod ops;
pub(crate) mod truncate;

/// Evaluates the temporal function `function` on its evaluated arguments.
pub(crate) fn call<En: Env>(
    function: TemporalFunction,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    match function {
        TemporalFunction::Construct(of) => construct::construct(of, args, env, span),
        TemporalFunction::Duration => construct::duration(args, env, span),
        TemporalFunction::Clock(of, clock) => construct::clock(of, clock, args, env, span),
        TemporalFunction::FromEpoch => construct::from_epoch(args, env, span),
        TemporalFunction::FromEpochMillis => construct::from_epoch_millis(args, env, span),
        TemporalFunction::Truncate(of) => truncate::truncate(of, args, env, span),
        TemporalFunction::Between(form) => between::between(form, args, env, span),
    }
}

/// The property access `value.key` on a temporal value (`d.year`, `dur.days`, ...).
pub(crate) fn component<E>(value: &Temporal, key: &str, span: Span) -> Result<Value<E>, EvalError> {
    ops::component(value, key, span)
}

/// The 0A000 error of a temporal form not supported yet: `what` names it (`date()`, `the
/// component year of a date`), so the message reads "`what` is not yet supported in this
/// version".
pub(crate) fn not_yet(what: &str, span: Span) -> EvalError {
    EvalError::new(
        status::NOT_SUPPORTED,
        format!("{what} is not yet supported in this version"),
        span,
    )
}

/// The Cypher name of `function` (`datetime`, not the ISO `zoned_datetime`) for the not-yet
/// messages: the openCypher TCK and Neo4j users write these, and a reason reads like the query.
pub(crate) fn cypher_name(function: TemporalFunction) -> &'static str {
    FUNCTIONS
        .iter()
        .find(|spec| spec.function == Function::Temporal(function))
        .and_then(|spec| spec.names.last().copied())
        .unwrap_or("function")
}

/// The name of a temporal value's kind in messages (`a date`, `a local time`, ...: the Cypher
/// type names, the engine `Time` being Cypher's `localtime`).
pub(crate) fn kind_name(value: &Temporal) -> &'static str {
    match value {
        Temporal::Date(_) => "a date",
        Temporal::LocalDateTime(_) => "a local datetime",
        Temporal::DateTime(_) => "a datetime",
        Temporal::Time(_) => "a local time",
        Temporal::Duration(_) => "a duration",
    }
}
