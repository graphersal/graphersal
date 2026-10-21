//! Truncation (3g.T7): `date.truncate`, `localtime.truncate`, `localdatetime.truncate`,
//! `datetime.truncate(unit, value [, map])` with Neo4j's units (`millennium` / `century` as the
//! TCK, U4; `weekYear`; `day` on a time), the map overriding fields of the truncated value or
//! naming a time zone. Rhai `date_trunc` keeps PostgreSQL's rules. Stub until 3g.T7.

use super::super::super::eval::{Env, EvalError, Value};
use super::super::super::lexer::Span;
use super::super::super::parser::{TemporalFunction, TemporalType};
use super::super::Arg;
use super::{cypher_name, not_yet};

/// `X.truncate(unit, value [, map])` for `X` of type `of`.
pub(crate) fn truncate<En: Env>(
    of: TemporalType,
    _args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let name = cypher_name(TemporalFunction::Truncate(of));
    Err(not_yet(&format!("{name}()"), span))
}
