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
//! Cypher's zoned `time()` (U2) and `X.realtime()` (U9) never reach this module: the parser
//! refuses them (`parser::NOT_YET_FUNCTIONS`). Values hold nanoseconds, as Neo4j's (U1).
//!
//! # Deliberate deviations from Neo4j (rule 18)
//!
//! Each one is a user decision, documented here, in the book (`gql/semantics.md`, section
//! "Where dates and times differ from Neo4j", and `dates/gql.md`) and, where the openCypher TCK
//! has cases for it, on the TCK compliance page through the exclusions of
//! `tests/opencypher/harness/scope.rs`:
//!
//! | Neo4j | `gql()` | Where |
//! |---|---|---|
//! | a `datetime` keeps its zone name (`+01:00[Europe/Stockholm]`), and later arithmetic steps over daylight saving | a zone name resolves to its offset at that instant and only the offset is kept (`x.timezone` is the offset text); TCK exclusion `zone-name-not-kept` (101 cases) | `construct.rs`, `ops.rs`, `between.rs`, `truncate.rs` |
//! | years -999 999 999 to 999 999 999 | the years -9999 to 9999, 22008 beyond (the engine range); TCK exclusion `year-range` (2 cases) | `construct.rs`, `between.rs` |
//! | `time()`, a time of day with an offset | 0A000 "not yet" (no engine type), `localtime()` / `datetime()` instead | `parser::NOT_YET_FUNCTIONS` |
//! | `X.realtime()` reads the live clock | refused like `rand()`; every other clock reads the query's one pinned instant | `parser::NOT_YET_FUNCTIONS`, `construct::clock` |
//!
//! Where GQL differs from the Gremlin / Rhai steps on the same engine values, it follows Neo4j:
//! `millennium` / `century` truncation starts at the years 2000 / 1900 (`date_trunc()`: 2001 /
//! 1901, `truncate.rs`), a date or time plus a duration ignores the parts its type lacks
//! (`date_add()` refuses them, `arith.rs`), Cypher `toString()` writes Neo4j's text while
//! `CAST(x AS STRING)` keeps the engine's ISO text (`ops::to_text`), `<` on two durations is
//! `NULL` (`eval.rs`), and `duration.between` aligns mixed kinds (`between.rs`).
//!
//! The family contract of [`super`] holds: the arity is the parser's, `NULL` propagates (F2),
//! a wrong kind is 22G03, no panics, exhaustive matches.

use super::super::eval::{Env, EvalError, Value, status};
use super::super::lexer::Span;
use super::super::parser::{FUNCTIONS, Function, TemporalFunction};
use super::Arg;
use crate::error::GqlHelpTopic;
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
    let (result, topic) = match function {
        TemporalFunction::Construct(of) => (
            construct::construct(of, args, env, span),
            GqlHelpTopic::TemporalConstruct,
        ),
        TemporalFunction::Duration => (
            construct::duration(args, env, span),
            GqlHelpTopic::TemporalConstruct,
        ),
        TemporalFunction::Clock(of, clock) => (
            construct::clock(of, clock, args, env, span),
            GqlHelpTopic::TemporalConstruct,
        ),
        TemporalFunction::FromEpoch => (
            construct::from_epoch(args, env, span),
            GqlHelpTopic::TemporalConstruct,
        ),
        TemporalFunction::FromEpochMillis => (
            construct::from_epoch_millis(args, env, span),
            GqlHelpTopic::TemporalConstruct,
        ),
        TemporalFunction::Truncate(of) => (
            truncate::truncate(of, args, env, span),
            GqlHelpTopic::TemporalTruncate,
        ),
        TemporalFunction::Between(form) => (
            between::between(form, args, env, span),
            GqlHelpTopic::TemporalBetween,
        ),
    };
    result.map_err(|error| advised(error, topic))
}

/// The property access `value.key` on a temporal value (`d.year`, `dur.days`, ...).
pub(crate) fn component<E>(value: &Temporal, key: &str, span: Span) -> Result<Value<E>, EvalError> {
    ops::component(value, key, span)
        .map_err(|error| advised(error, GqlHelpTopic::TemporalComponent))
}

/// `error` with the `help()` advice of its function family (rule 6): `topic` for a wrong kind
/// (22G03) and, for `X.truncate()`, a unit it does not take (22023). The text and range errors
/// (22007 / 22008) keep the advice of their GQLSTATUS, an error that chose its own topic keeps
/// it.
fn advised(error: EvalError, topic: GqlHelpTopic) -> EvalError {
    let applies = error.gqlstatus == status::INVALID_VALUE_TYPE
        || (error.gqlstatus == status::INVALID_PARAMETER
            && topic == GqlHelpTopic::TemporalTruncate);
    if applies && error.topic == GqlHelpTopic::Kind {
        error.with_topic(topic)
    } else {
        error
    }
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
