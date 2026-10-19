//! The scalar functions of stage 1c: a pure stage between the evaluator and the values, one file
//! per family. The user documentation is the book: `gql/subset.md` ("Functions", one table per
//! family), `gql/semantics.md` ("Functions": the rules cited here as F1-F13, strict `CAST` vs
//! lenient `toX`, rounding, where they differ from Neo4j) and `gql/aliases.md` ("Compatibility
//! functions": the openCypher functions without a GQL counterpart).
//!
//! [`eval`](super::eval) evaluates every argument of a call (left to right; only `coalesce` is
//! lazy, and it stays in the evaluator with the stage 1a functions) and hands the values to
//! [`call`], which dispatches by [`family`]:
//!
//! | Family | File | Entry |
//! |---|---|---|
//! | conversions: `CAST`, `toInteger`, `toFloat`, `toBoolean`, `toString` and their `...OrNull` forms | `convert.rs` | [`convert::convert`] |
//! | strings: `left`, `right`, `substring`, `trim` (strings), `btrim` / `ltrim` / `rtrim`, `replace`, `split`, `reverse`, `byte_length`, `isEmpty`, `nullif`; the SQL `TRIM` forms | `strings.rs` | [`strings::call`], [`strings::trim`] |
//! | numbers: `abs`, `floor`, `ceil`, `round`, `sign`, `sqrt`, `exp`, `ln`, `log`, `log10`, `power`, `mod` | `numbers.rs` | [`numbers::call`] |
//! | lists, elements, paths: `head`, `last`, `tail`, `trim(list, n)`, `range`, `keys`, `properties`, `startNode`, `endNode`, `nodes`, `relationships`; `IS [NOT] SOURCE OF` / `DESTINATION OF` | `lists.rs` | [`lists::call`], [`lists::is_endpoint`] |
//!
//! `trim` is dispatched by its first argument (design F13): a list goes to `lists.rs`, anything
//! else to `strings.rs`.
//!
//! # Contract of a family file
//!
//! - The arity is checked by the parser (`FUNCTIONS`): an entry receives between `min_args` and
//!   `max_args` arguments. Each [`Arg`] carries the span of its expression; `span` is the whole
//!   call. Errors point at the offending argument where there is one.
//! - NULL propagates (design F2): any `NULL` argument gives `NULL` before any type check, except
//!   `nullif` and the `...OrNull` forms ([`any_null`]).
//! - A wrong kind is 22G03 ([`wrong_kind`]), never a silent `NULL` (except `...OrNull`); an
//!   integer argument must be an integer ([`integer_arg`], F3).
//! - Characters are Unicode scalar values (F1): slice by `char_indices`, never `&s[a..b]` on a
//!   computed offset.
//! - A result larger than the inputs asks [`Env::reserve`] first ([`reserve`], F6); a refusal
//!   is the [`status::LIMIT`] error, which a traversal step replaces by the row's limit error.
//! - No panics, exhaustive matches on [`Function`] (each family lists its own variants and maps
//!   the others to [`not_implemented`]), status codes from [`status`].

use super::ast::{CastTarget, Endpoint, TrimSide};
use super::eval::{Env, EvalError, Value, function_name, status};
use super::lexer::Span;
use super::parser::Function;
use crate::ElementProperty;
use crate::value::number::{N, Number};

pub(crate) mod convert;
pub(crate) mod lists;
pub(crate) mod numbers;
pub(crate) mod strings;

#[cfg(test)]
pub(crate) mod test_env;

/// One evaluated argument with the span of its expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Arg<E> {
    /// The value.
    pub(crate) value: Value<E>,
    /// The argument expression.
    pub(crate) span: Span,
}

impl<E> Arg<E> {
    /// Whether the value is `NULL`.
    #[allow(dead_code, reason = "stage 1c scaffolding, used by T2-T5")]
    pub(crate) fn is_null(&self) -> bool {
        self.value.is_null()
    }
}

/// Where a function is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    /// The stage 1a scalar functions, in `eval.rs`.
    Core,
    /// The aggregates (`aggregate.rs`).
    Aggregate,
    /// `CAST`: compiled to its own node with the target type (never a plain call).
    Cast,
    /// A Cypher conversion function (`convert.rs`).
    Convert(Conversion),
    /// `strings.rs` (and `trim`, see the module docs).
    Strings,
    /// `numbers.rs`.
    Numbers,
    /// `lists.rs`.
    Lists,
}

/// How a conversion treats a value it cannot convert (design K-D22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// GQL `CAST`: an unconvertible string is 22018, a wrong kind 22G03.
    Cast,
    /// Cypher `toX`: an unconvertible string is `NULL`, a wrong kind 22G03.
    Lenient,
    /// Cypher `toXOrNull`: every failure is `NULL` (except an element or a path given to
    /// `toStringOrNull`, design F10).
    OrNull,
}

/// A conversion: the target type and the mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Conversion {
    /// The target type.
    pub(crate) target: CastTarget,
    /// Strict, lenient or `NULL` on failure.
    pub(crate) mode: Mode,
}

impl Conversion {
    /// The function as written in messages: `CAST(.. AS INTEGER)`, `toInteger()`, ...
    pub(crate) fn name(self) -> &'static str {
        match (self.mode, self.target) {
            (Mode::Cast, CastTarget::Integer) => "CAST(.. AS INTEGER)",
            (Mode::Cast, CastTarget::Float) => "CAST(.. AS FLOAT)",
            (Mode::Cast, CastTarget::String) => "CAST(.. AS STRING)",
            (Mode::Cast, CastTarget::Boolean) => "CAST(.. AS BOOLEAN)",
            (Mode::Lenient, CastTarget::Integer) => "toInteger()",
            (Mode::Lenient, CastTarget::Float) => "toFloat()",
            (Mode::Lenient, CastTarget::String) => "toString()",
            (Mode::Lenient, CastTarget::Boolean) => "toBoolean()",
            (Mode::OrNull, CastTarget::Integer) => "toIntegerOrNull()",
            (Mode::OrNull, CastTarget::Float) => "toFloatOrNull()",
            (Mode::OrNull, CastTarget::String) => "toStringOrNull()",
            (Mode::OrNull, CastTarget::Boolean) => "toBooleanOrNull()",
        }
    }
}

const fn conversion(target: CastTarget, mode: Mode) -> Family {
    Family::Convert(Conversion { target, mode })
}

/// The family of `function` (exhaustive: a new variant must be placed here).
pub(crate) fn family(function: Function) -> Family {
    match function {
        Function::ElementId
        | Function::Labels
        | Function::Type
        | Function::Coalesce
        | Function::Upper
        | Function::Lower
        | Function::CharLength
        | Function::Size
        | Function::Cardinality
        | Function::PathLength => Family::Core,
        Function::Count
        | Function::Sum
        | Function::Avg
        | Function::Min
        | Function::Max
        | Function::CollectList => Family::Aggregate,
        Function::Cast => Family::Cast,
        Function::ToInteger => conversion(CastTarget::Integer, Mode::Lenient),
        Function::ToIntegerOrNull => conversion(CastTarget::Integer, Mode::OrNull),
        Function::ToFloat => conversion(CastTarget::Float, Mode::Lenient),
        Function::ToFloatOrNull => conversion(CastTarget::Float, Mode::OrNull),
        Function::ToBoolean => conversion(CastTarget::Boolean, Mode::Lenient),
        Function::ToBooleanOrNull => conversion(CastTarget::Boolean, Mode::OrNull),
        Function::ToString => conversion(CastTarget::String, Mode::Lenient),
        Function::ToStringOrNull => conversion(CastTarget::String, Mode::OrNull),
        Function::Left
        | Function::Right
        | Function::Substring
        | Function::Trim
        | Function::Btrim
        | Function::Ltrim
        | Function::Rtrim
        | Function::Replace
        | Function::Split
        | Function::Reverse
        | Function::ByteLength
        | Function::IsEmpty
        | Function::NullIf => Family::Strings,
        Function::Abs
        | Function::Floor
        | Function::Ceil
        | Function::Round
        | Function::Sign
        | Function::Sqrt
        | Function::Exp
        | Function::Ln
        | Function::Log
        | Function::Log10
        | Function::Power
        | Function::Mod => Family::Numbers,
        Function::Head
        | Function::Last
        | Function::Tail
        | Function::Range
        | Function::Keys
        | Function::Properties
        | Function::StartNode
        | Function::EndNode
        | Function::Nodes
        | Function::Relationships => Family::Lists,
    }
}

/// Evaluates a stage 1c function on its evaluated arguments (see the module docs).
pub(crate) fn call<En: Env>(
    function: Function,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    match family(function) {
        Family::Convert(conversion) => match args.into_iter().next() {
            Some(arg) => convert::convert(conversion, arg, env, span),
            None => Ok(Value::Null),
        },
        Family::Strings
            if function == Function::Trim
                && matches!(
                    args.first(),
                    Some(Arg {
                        value: Value::List(_),
                        ..
                    })
                ) =>
        {
            lists::call(function, args, env, span)
        }
        Family::Strings => strings::call(function, args, env, span),
        Family::Numbers => numbers::call(function, args, env, span),
        Family::Lists => lists::call(function, args, env, span),
        Family::Core | Family::Aggregate | Family::Cast => Err(EvalError::new(
            status::SEMANTIC,
            format!("{}() is not evaluated here", function_name(function)),
            span,
        )),
    }
}

/// `CAST(arg AS target)`.
pub(crate) fn cast<En: Env>(
    target: CastTarget,
    arg: Arg<En::Element>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let conversion = Conversion {
        target,
        mode: Mode::Cast,
    };
    convert::convert(conversion, arg, env, span)
}

/// The SQL forms of `TRIM` (`character`: `None` for the default, the space).
pub(crate) fn trim<En: Env>(
    side: TrimSide,
    character: Option<Arg<En::Element>>,
    source: Arg<En::Element>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    strings::trim(side, character, source, env, span)
}

/// `operand IS [NOT] SOURCE OF edge` / `IS [NOT] DESTINATION OF edge`.
pub(crate) fn is_endpoint<En: Env>(
    operand: &Value<En::Element>,
    edge: &Value<En::Element>,
    end: Endpoint,
    negated: bool,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    lists::is_endpoint(operand, edge, end, negated, env, span)
}

// ----- shared helpers of the family files -----

/// The 0A000 error of a function whose family is not implemented yet.
pub(crate) fn not_implemented(name: &str, span: Span) -> EvalError {
    EvalError::new(
        status::NOT_SUPPORTED,
        format!("{name} is not implemented yet"),
        span,
    )
}

/// The 0A000 error of a [`Function`] whose family is not implemented yet.
pub(crate) fn function_not_implemented(function: Function, span: Span) -> EvalError {
    not_implemented(&format!("{}()", function_name(function)), span)
}

/// Whether any argument is `NULL` (design F2).
#[allow(dead_code, reason = "stage 1c scaffolding, used by T2-T5")]
pub(crate) fn any_null<E>(args: &[Arg<E>]) -> bool {
    args.iter().any(Arg::is_null)
}

/// The 22G03 error "`name()` takes `what`, not <kind of `arg`>", at the argument.
#[allow(dead_code, reason = "stage 1c scaffolding, used by T2-T5")]
pub(crate) fn wrong_kind<E>(function: Function, what: &str, arg: &Arg<E>) -> EvalError {
    EvalError::invalid_type(
        format!(
            "{}() takes {what}, not {}",
            function_name(function),
            arg.value.kind_name()
        ),
        arg.span,
    )
}

/// The argument as text, else 22G03 naming `what` (`"a string"`, ...).
#[allow(dead_code, reason = "stage 1c scaffolding, used by T2-T5")]
pub(crate) fn string_arg<'a, E>(
    function: Function,
    arg: &'a Arg<E>,
    what: &str,
) -> Result<&'a str, EvalError> {
    arg.value
        .as_str()
        .ok_or_else(|| wrong_kind(function, what, arg))
}

/// The argument as a number, else 22G03 naming `what`.
#[allow(dead_code, reason = "stage 1c scaffolding, used by T2-T5")]
pub(crate) fn number_arg<'a, E>(
    function: Function,
    arg: &'a Arg<E>,
    what: &str,
) -> Result<&'a Number, EvalError> {
    match &arg.value {
        Value::Scalar(ElementProperty::Number(number)) => Ok(number),
        _ => Err(wrong_kind(function, what, arg)),
    }
}

/// The argument as an integer (a float is 22G03 too, design F3), else 22G03 naming `what`.
#[allow(dead_code, reason = "stage 1c scaffolding, used by T2-T5")]
pub(crate) fn integer_arg<E>(
    function: Function,
    arg: &Arg<E>,
    what: &str,
) -> Result<i64, EvalError> {
    match number_arg(function, arg, what)?.n {
        N::NegInt(value) => Ok(value),
        N::Float(_) => Err(wrong_kind(function, what, arg)),
    }
}

/// Asks `env` for `bytes` (design F6); a refusal is the [`status::LIMIT`] error at `span`.
pub(crate) fn reserve<En: Env>(env: &En, bytes: u64, span: Span) -> Result<(), EvalError> {
    if env.reserve(bytes) {
        Ok(())
    } else {
        Err(EvalError::new(
            status::LIMIT,
            format!("the result would need {bytes} bytes, more than the materialization limit"),
            span,
        ))
    }
}

/// The estimated size of a list of `items` values (for [`reserve`]).
pub(crate) fn list_bytes(items: u64) -> u64 {
    items.saturating_mul(std::mem::size_of::<Value<()>>() as u64)
}

#[cfg(test)]
mod tests {
    use super::super::parser::{FUNCTIONS, FunctionKind};
    use super::test_env::*;
    use super::*;

    /// Every GQLSTATUS a function may raise (the family files add no other).
    const KNOWN: &[&str] = &[
        status::DIVISION_BY_ZERO,
        status::OUT_OF_RANGE,
        status::INVALID_VALUE_TYPE,
        status::NOT_SUPPORTED,
        status::INVALID_CAST,
        status::SUBSTRING,
        status::TRIM,
        status::LOGARITHM,
        status::POWER,
        status::INVALID_PARAMETER,
        status::LIMIT,
    ];

    #[test]
    fn every_function_has_its_family() {
        for spec in FUNCTIONS {
            let family = family(spec.function);
            match spec.kind {
                FunctionKind::Aggregate => assert_eq!(family, Family::Aggregate, "{}", spec.name()),
                FunctionKind::Scalar => {
                    assert_ne!(family, Family::Aggregate, "{}", spec.name());
                }
            }
            if let Family::Convert(conversion) = family {
                let name = conversion.name();
                assert_eq!(name, format!("{}()", spec.names[0]), "{}", spec.name());
            }
        }
        assert_eq!(family(Function::Trim), Family::Strings);
        assert_eq!(family(Function::Nodes), Family::Lists);
        assert_eq!(
            Conversion {
                target: CastTarget::Float,
                mode: Mode::Cast
            }
            .name(),
            "CAST(.. AS FLOAT)"
        );
    }

    #[test]
    fn every_argument_is_evaluated_before_the_call() {
        // An error in any argument surfaces, whatever the function does with it.
        check_statuses(&[
            ("left('a', 1 / 0)", status::DIVISION_BY_ZERO),
            ("replace('a', 'b', 'c', 1 / 0)", status::DIVISION_BY_ZERO),
            ("round(1.5, 1, 1 / 0)", status::DIVISION_BY_ZERO),
            ("range(1 / 0, 2)", status::DIVISION_BY_ZERO),
            ("nullif(1, 1 / 0)", status::DIVISION_BY_ZERO),
            ("toInteger(1 / 0)", status::DIVISION_BY_ZERO),
            ("CAST(1 / 0 AS INTEGER)", status::DIVISION_BY_ZERO),
            ("TRIM(1 / 0 FROM 'x')", status::DIVISION_BY_ZERO),
            ("TRIM(LEADING 'x' FROM 1 / 0)", status::DIVISION_BY_ZERO),
            ("n IS SOURCE OF 1 / 0", status::DIVISION_BY_ZERO),
            ("(1 / 0) IS NOT DESTINATION OF e", status::DIVISION_BY_ZERO),
        ]);
    }

    #[test]
    fn concat_reserves_its_result() {
        let tight = TestRow::with_limit(3);
        let error = eval_in(&tight, "'ab' || 'cd'").unwrap_err();
        assert_eq!(error.gqlstatus, status::LIMIT);
        assert_eq!(
            eval_in(&TestRow::with_limit(4), "'ab' + 'cd'"),
            Ok(string("abcd"))
        );
        let error = eval_in(&tight, "[1] || [2]").unwrap_err();
        assert_eq!(error.gqlstatus, status::LIMIT);
        assert_eq!(value("[1] + [2]"), list(vec![int(1), int(2)]));
        // Nothing to reserve for NULL or numbers.
        assert_eq!(eval_in(&tight, "1 + 2"), Ok(int(3)));
        assert_eq!(eval_in(&tight, "'abcd' || NULL"), Ok(null()));
    }

    /// A tiny deterministic generator for the argument combinations.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as usize) % n.max(1)
        }
    }

    #[test]
    fn stage_1c_calls_never_panic() {
        const POOL: &[&str] = &[
            "NULL",
            "0",
            "1",
            "-1",
            "2.5",
            "-0.5",
            "$big",
            "$min",
            "'héllo'",
            "''",
            "'日本語 🎉'",
            "' 42 '",
            "'1e3'",
            "'NaN'",
            "'true'",
            "TRUE",
            "[1, 'a', NULL]",
            "[]",
            "{a: 1}",
            "n",
            "e",
            "p",
            "z",
            "x",
            "1.0 / 3",
        ];
        let mut rng = Rng(0x1C1C_5EED);
        for spec in FUNCTIONS {
            if matches!(family(spec.function), Family::Core | Family::Aggregate) {
                continue;
            }
            let name = spec.names[0];
            for arity in spec.min_args..=spec.max_args.min(4) {
                for _ in 0..60 {
                    let args: Vec<&str> = (0..arity).map(|_| POOL[rng.below(POOL.len())]).collect();
                    let call = if spec.function == Function::Cast {
                        let target = ["INTEGER", "FLOAT", "STRING", "BOOLEAN"][rng.below(4)];
                        format!("CAST({} AS {target})", args.join(", "))
                    } else {
                        format!("{name}({})", args.join(", "))
                    };
                    if let Err(error) = eval(&call) {
                        assert!(KNOWN.contains(&error.gqlstatus), "{call}: {error:?}");
                    }
                }
            }
        }
        for (end, operand, edge) in [
            ("SOURCE", "n", "e"),
            ("DESTINATION", "m", "e"),
            ("SOURCE", "e", "n"),
            ("DESTINATION", "x", "e"),
        ] {
            let text = format!("{operand} IS {end} OF {edge}");
            if let Err(error) = eval(&text) {
                assert!(KNOWN.contains(&error.gqlstatus), "{text}: {error:?}");
            }
        }
        for side in ["LEADING", "TRAILING", "BOTH", ""] {
            for (character, source) in [("'x'", "'xax'"), ("NULL", "'a'"), ("'xy'", "'a'")] {
                let text = format!("TRIM({side} {character} FROM {source})");
                if let Err(error) = eval(&text) {
                    assert!(KNOWN.contains(&error.gqlstatus), "{text}: {error:?}");
                }
            }
        }
    }
}
