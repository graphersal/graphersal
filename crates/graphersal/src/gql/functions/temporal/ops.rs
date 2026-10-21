//! Component access and text (3g.T5): `x.year`, `x.week`, `x.millisecond`, `x.offset`,
//! `x.epochSeconds`, the duration totals (`dur.months`, `dur.seconds`) and remainders
//! (`dur.monthsOfYear`, `dur.nanosecondsOfSecond`) with Neo4j's names and meanings (U11, U12);
//! `toString(x)` in the Neo4j / `java.time` text form (U6; `CAST(x AS STRING)` keeps the engine
//! text, and the shared `ElementProperty::cast` is never changed). Stubs until 3g.T5.

use super::super::super::eval::{EvalError, Value};
use super::super::super::lexer::Span;
use super::{kind_name, not_yet};
use crate::CompactString;
use crate::value::temporal::Temporal;

/// The component `key` of `value` (`NULL` for an unknown key of a map-like value is Neo4j's
/// rule for maps only; an unknown component is an error).
pub(crate) fn component<E>(value: &Temporal, key: &str, span: Span) -> Result<Value<E>, EvalError> {
    Err(not_yet(
        &format!("the component {key} of {}", kind_name(value)),
        span,
    ))
}

/// The Neo4j text of `value` for Cypher `toString(x)`.
#[allow(dead_code, reason = "wired into convert.rs by 3g.T5")]
pub(crate) fn to_text(value: &Temporal, span: Span) -> Result<CompactString, EvalError> {
    Err(not_yet(
        &format!("toString() of {}", kind_name(value)),
        span,
    ))
}
