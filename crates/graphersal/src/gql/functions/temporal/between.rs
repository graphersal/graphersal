//! Differences (3g.T8): `duration.between(a, b)` (Neo4j's months, days, rest algorithm) and
//! `duration.inMonths` / `inDays` / `inSeconds`, over any pair of temporal instants (a missing
//! date gives the times only, a missing time midnight, a missing offset the other side's, U15).
//! Rhai `age` / `date_diff` keep their rules. Stub until 3g.T8.

use super::super::super::eval::{Env, EvalError, Value};
use super::super::super::lexer::Span;
use super::super::super::parser::{Between, TemporalFunction};
use super::super::Arg;
use super::{cypher_name, not_yet};

/// `duration.between(a, b)` or one of its `in*` forms (`form`).
pub(crate) fn between<En: Env>(
    form: Between,
    _args: Vec<Arg<En::Element>>,
    _env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let name = cypher_name(TemporalFunction::Between(form));
    Err(not_yet(&format!("{name}()"), span))
}
