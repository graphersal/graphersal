//! Numeric functions: `abs`, `floor`, `ceil` / `ceiling`, `round`, `sign`, `sqrt`,
//! `exp`, `ln`, `log` (one argument: natural; two: GQL `LOG(base, x)`), `log10`, `power`, `mod`.
//!
//! The contract of a family file is in the module docs of `functions/mod.rs`; unit tests use
//! the test `Env` and helpers of `functions/test_env.rs`.
//!
//! Result types (design F5): `abs`, `floor` and `ceil` keep the argument's type; `round` is always
//! a float (Neo4j); `sign` is an integer; `sqrt`, `exp`, `ln`, `log`, `log10` and `power` are
//! floats; `mod` is exactly `%`. A finite input never gives a non-finite result (F4): such a
//! result is an error instead (22003, 2201E or 2201F); a `NaN` or an infinity that is already
//! stored passes through.

use std::fmt::{self, Write as _};

use super::super::eval::{Env, EvalError, Value, float as to_f64, status};
use super::super::lexer::Span;
use super::super::parser::Function;
use super::{Arg, any_null, function_not_implemented, integer_arg, number_arg, string_arg};
use crate::ElementProperty;
use crate::value::number::{N, Number};

/// What the numeric arguments are called in messages.
const A_NUMBER: &str = "a number";

/// Evaluates the numeric function `function` (its family, [`super::Family::Numbers`]) on its
/// evaluated arguments.
pub(crate) fn call<En: Env>(
    function: Function,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let _ = env;
    match function {
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
        | Function::Mod => {}
        Function::ElementId
        | Function::Labels
        | Function::Type
        | Function::Coalesce
        | Function::Upper
        | Function::Lower
        | Function::CharLength
        | Function::Size
        | Function::Cardinality
        | Function::PathLength
        | Function::Count
        | Function::Sum
        | Function::Avg
        | Function::Min
        | Function::Max
        | Function::CollectList
        | Function::Cast
        | Function::ToInteger
        | Function::ToIntegerOrNull
        | Function::ToFloat
        | Function::ToFloatOrNull
        | Function::ToBoolean
        | Function::ToBooleanOrNull
        | Function::ToString
        | Function::ToStringOrNull
        | Function::Left
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
        | Function::NullIf
        | Function::Head
        | Function::Last
        | Function::Tail
        | Function::Range
        | Function::Keys
        | Function::Properties
        | Function::StartNode
        | Function::EndNode
        | Function::Nodes
        | Function::Relationships => return Err(function_not_implemented(function, span)),
    }
    // F2: NULL in, NULL out, before any type check.
    if any_null(&args) {
        return Ok(Value::Null);
    }
    let first = arg(&args, 0, function, span)?;
    match function {
        Function::Abs => match number_arg(function, first, A_NUMBER)?.n {
            N::NegInt(value) => value
                .checked_abs()
                .map(Value::integer)
                .ok_or_else(|| EvalError::out_of_range(span)),
            N::Float(value) => Ok(Value::float(value.0.abs())),
        },
        Function::Floor => match number_arg(function, first, A_NUMBER)?.n {
            N::NegInt(value) => Ok(Value::integer(value)),
            N::Float(value) => Ok(Value::float(value.0.floor())),
        },
        Function::Ceil => match number_arg(function, first, A_NUMBER)?.n {
            N::NegInt(value) => Ok(Value::integer(value)),
            N::Float(value) => Ok(Value::float(value.0.ceil())),
        },
        Function::Sign => {
            let sign = match number_arg(function, first, A_NUMBER)?.n {
                N::NegInt(value) => value.signum(),
                // `f64::signum` gives 1.0 for 0.0 and NaN for NaN; `sign(NaN)` is 0 (Neo4j).
                N::Float(value) if value.0 > 0.0 => 1,
                N::Float(value) if value.0 < 0.0 => -1,
                N::Float(_) => 0,
            };
            Ok(Value::integer(sign))
        }
        Function::Round => round(&args, span),
        Function::Sqrt => {
            let x = float_arg(function, first)?;
            if x < 0.0 {
                return Err(EvalError::new(
                    status::POWER,
                    format!("sqrt() of a negative number ({})", Number::from_f64(x)),
                    first.span,
                ));
            }
            finite_result(x.sqrt(), &[x], span)
        }
        Function::Exp => {
            let x = float_arg(function, first)?;
            finite_result(x.exp(), &[x], span)
        }
        Function::Ln => {
            let x = log_argument(function, first)?;
            finite_result(x.ln(), &[x], span)
        }
        Function::Log10 => {
            let x = log_argument(function, first)?;
            finite_result(x.log10(), &[x], span)
        }
        Function::Log => match args.get(1) {
            // One argument: Cypher's natural logarithm (design F12).
            None => {
                let x = log_argument(function, first)?;
                finite_result(x.ln(), &[x], span)
            }
            // Two arguments: GQL `LOG(base, x)`.
            Some(second) => {
                let base = float_arg(function, first)?;
                float_arg(function, second)?;
                if base <= 0.0 || base == 1.0 {
                    return Err(EvalError::new(
                        status::LOGARITHM,
                        format!(
                            "log(base, x) needs a base > 0 and <> 1, not {}",
                            Number::from_f64(base)
                        ),
                        first.span,
                    ));
                }
                let x = log_argument(function, second)?;
                // Bases 2 and 10 exactly (`log(10, 0.001)` is -3, not -2.9999999999999996).
                let result = if base == 2.0 {
                    x.log2()
                } else if base == 10.0 {
                    x.log10()
                } else {
                    x.ln() / base.ln()
                };
                finite_result(result, &[base, x], span)
            }
        },
        Function::Power => {
            let second = arg(&args, 1, function, span)?;
            let x = float_arg(function, first)?;
            let y = float_arg(function, second)?;
            if x == 0.0 && y < 0.0 {
                return Err(EvalError::new(
                    status::POWER,
                    format!(
                        "power() of 0 to a negative exponent ({})",
                        Number::from_f64(y)
                    ),
                    span,
                ));
            }
            if x < 0.0 && y.is_finite() && y.fract() != 0.0 {
                return Err(EvalError::new(
                    status::POWER,
                    format!(
                        "power() of a negative base ({}) to a non-integer exponent ({})",
                        Number::from_f64(x),
                        Number::from_f64(y)
                    ),
                    span,
                ));
            }
            finite_result(x.powf(y), &[x, y], span)
        }
        Function::Mod => {
            let second = arg(&args, 1, function, span)?;
            let a = number_arg(function, first, A_NUMBER)?;
            let b = number_arg(function, second, A_NUMBER)?;
            modulo(a, b, second.span)
        }
        // Not numeric: refused above.
        Function::ElementId
        | Function::Labels
        | Function::Type
        | Function::Coalesce
        | Function::Upper
        | Function::Lower
        | Function::CharLength
        | Function::Size
        | Function::Cardinality
        | Function::PathLength
        | Function::Count
        | Function::Sum
        | Function::Avg
        | Function::Min
        | Function::Max
        | Function::CollectList
        | Function::Cast
        | Function::ToInteger
        | Function::ToIntegerOrNull
        | Function::ToFloat
        | Function::ToFloatOrNull
        | Function::ToBoolean
        | Function::ToBooleanOrNull
        | Function::ToString
        | Function::ToStringOrNull
        | Function::Left
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
        | Function::NullIf
        | Function::Head
        | Function::Last
        | Function::Tail
        | Function::Range
        | Function::Keys
        | Function::Properties
        | Function::StartNode
        | Function::EndNode
        | Function::Nodes
        | Function::Relationships => Err(function_not_implemented(function, span)),
    }
}

/// The argument at `index` (the parser checked the arity; a missing one is an error, never a
/// panic).
fn arg<E>(
    args: &[Arg<E>],
    index: usize,
    function: Function,
    span: Span,
) -> Result<&Arg<E>, EvalError> {
    args.get(index).ok_or_else(|| {
        EvalError::new(
            status::INVALID_PARAMETER,
            format!(
                "{}() is missing its argument {}",
                super::super::eval::function_name(function),
                index + 1
            ),
            span,
        )
    })
}

/// The argument as a float (an integer converted), else 22G03.
fn float_arg<E>(function: Function, arg: &Arg<E>) -> Result<f64, EvalError> {
    number_arg(function, arg, A_NUMBER).map(to_f64)
}

/// The argument of a logarithm: a number > 0 (2201E otherwise; `NaN` passes).
fn log_argument<E>(function: Function, arg: &Arg<E>) -> Result<f64, EvalError> {
    let x = float_arg(function, arg)?;
    if x <= 0.0 {
        return Err(EvalError::new(
            status::LOGARITHM,
            format!(
                "{}() of a number <= 0 ({})",
                super::super::eval::function_name(function),
                Number::from_f64(x)
            ),
            arg.span,
        ));
    }
    Ok(x)
}

/// `result` as a float value; a non-finite result of finite `inputs` is 22003 (design F4).
fn finite_result<E>(result: f64, inputs: &[f64], span: Span) -> Result<Value<E>, EvalError> {
    if !result.is_finite() && inputs.iter().all(|x| x.is_finite()) {
        return Err(EvalError::out_of_range(span));
    }
    Ok(Value::float(result))
}

/// `mod(a, b)`: `a % b` (an integer pair stays integer; `b = 0` is 22012 at `span`, the
/// divisor, also for floats: ISO GQL's `MOD`, where the Cypher operator `%` follows IEEE 754).
fn modulo<E>(a: &Number, b: &Number, span: Span) -> Result<Value<E>, EvalError> {
    match (&a.n, &b.n) {
        (N::NegInt(_), N::NegInt(0)) => Err(EvalError::division_by_zero(span)),
        // `i64::MIN % -1` is 0 mathematically; only `checked_rem` reports it as overflow.
        (N::NegInt(a), N::NegInt(b)) => Ok(Value::integer(a.checked_rem(*b).unwrap_or(0))),
        _ => {
            let (a, b) = (to_f64(a), to_f64(b));
            if b == 0.0 {
                return Err(EvalError::division_by_zero(span));
            }
            Ok(Value::Scalar(ElementProperty::Number(Number::from_f64(
                a % b,
            ))))
        }
    }
}

// ----- round -----

/// A rounding mode of `round(x, precision, mode)` (Java `RoundingMode` without `UNNECESSARY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoundingMode {
    /// Away from zero.
    Up,
    /// Toward zero.
    Down,
    /// Toward positive infinity.
    Ceiling,
    /// Toward negative infinity.
    Floor,
    /// To the nearest, a tie away from zero (the default).
    HalfUp,
    /// To the nearest, a tie toward zero.
    HalfDown,
    /// To the nearest, a tie to the even neighbour.
    HalfEven,
}

/// The modes as written (case-sensitive, Java `RoundingMode.valueOf`).
const MODES: &[(&str, RoundingMode)] = &[
    ("UP", RoundingMode::Up),
    ("DOWN", RoundingMode::Down),
    ("CEILING", RoundingMode::Ceiling),
    ("FLOOR", RoundingMode::Floor),
    ("HALF_UP", RoundingMode::HalfUp),
    ("HALF_DOWN", RoundingMode::HalfDown),
    ("HALF_EVEN", RoundingMode::HalfEven),
];

/// The mode named by `arg`: one of [`MODES`], else 22023 (`'UNNECESSARY'` included).
fn rounding_mode<E>(arg: &Arg<E>) -> Result<RoundingMode, EvalError> {
    let text = string_arg(Function::Round, arg, "a rounding mode string")?;
    if let Some((_, mode)) = MODES.iter().find(|(name, _)| *name == text) {
        return Ok(*mode);
    }
    let message = if text == "UNNECESSARY" {
        "round() does not support the mode 'UNNECESSARY'; use one of 'UP', 'DOWN', 'CEILING', \
         'FLOOR', 'HALF_UP', 'HALF_DOWN', 'HALF_EVEN'"
            .to_string()
    } else {
        format!(
            "round() has no rounding mode {text:?}; use one of 'UP', 'DOWN', 'CEILING', 'FLOOR', \
             'HALF_UP', 'HALF_DOWN', 'HALF_EVEN' (upper case)"
        )
    };
    Err(EvalError::new(status::INVALID_PARAMETER, message, arg.span))
}

/// `round(x [, precision [, mode]])` (Neo4j, always a float): one argument, or precision 0
/// without a mode, is Java `Math.round` (a tie toward positive infinity); otherwise decimal
/// rounding of the shortest decimal text of `x` (Java `BigDecimal.valueOf`) to `precision`
/// decimals with `mode` (`HALF_UP` by default). A negative precision is 22023; NaN and the
/// infinities pass through.
fn round<E>(args: &[Arg<E>], span: Span) -> Result<Value<E>, EvalError> {
    let first = arg(args, 0, Function::Round, span)?;
    let x = float_arg(Function::Round, first)?;
    let precision = match args.get(1) {
        Some(second) => {
            let precision = integer_arg(Function::Round, second, "an integer precision")?;
            if precision < 0 {
                return Err(EvalError::new(
                    status::INVALID_PARAMETER,
                    format!("round() needs a precision >= 0, not {precision}"),
                    second.span,
                ));
            }
            precision
        }
        None => 0,
    };
    let mode = args.get(2).map(rounding_mode).transpose()?;
    if !x.is_finite() {
        return Ok(Value::float(x));
    }
    match mode {
        None if precision == 0 => Ok(Value::float(java_math_round(x))),
        _ => decimal_round(x, precision, mode.unwrap_or(RoundingMode::HalfUp))
            .map(Value::float)
            .ok_or_else(|| EvalError::out_of_range(span)),
    }
}

/// Java `Math.round` as a float: the nearest integer, a tie toward positive infinity, without
/// Java's saturation at the `long` range (a float that large is already an integer).
fn java_math_round(x: f64) -> f64 {
    let floor = x.floor();
    // Exact: below 2^52 a float and its floor share the exponent range, above it `x` is integral.
    if x - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// A fixed stack buffer for the decimal texts of [`decimal_round`] (no allocation per row).
struct Text {
    bytes: [u8; 64],
    len: usize,
}

impl Text {
    fn new() -> Self {
        Text {
            bytes: [0; 64],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }
}

impl fmt::Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len.checked_add(s.len()).ok_or(fmt::Error)?;
        let target = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        target.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// `x` (finite) rounded to `precision` (>= 0) decimals with `mode`, on the shortest decimal
/// text of `x` (Rust's `{:e}` is the shortest text that reads back as `x`, like Java's
/// `Double.toString` since JDK 19). `None` only if a text does not fit the buffer, which the
/// bounds below rule out.
fn decimal_round(x: f64, precision: i64, mode: RoundingMode) -> Option<f64> {
    let mut text = Text::new();
    write!(text, "{:e}", x.abs()).ok()?;
    // `d.ddde<exp>`: at most 17 significant digits.
    let bytes = text.as_bytes();
    let e_at = bytes.iter().position(|&b| b == b'e')?;
    let (mantissa, exponent) = (bytes.get(..e_at)?, bytes.get(e_at + 1..)?);
    let exponent: i64 = std::str::from_utf8(exponent).ok()?.parse().ok()?;
    let mut digits = [0u8; 20];
    let mut count = 0usize;
    for &b in mantissa.iter().filter(|b| b.is_ascii_digit()) {
        *digits.get_mut(count)? = b - b'0';
        count += 1;
    }
    let digits = digits.get(..count)?;
    // Digit `i` has the place value 10^(exponent - i); the kept digits are those with a place
    // value >= 10^-precision.
    let kept = exponent.saturating_add(precision).saturating_add(1);
    if kept >= count as i64 {
        return Some(x);
    }
    let kept = usize::try_from(kept).unwrap_or(0);
    let mut integer: u64 = 0;
    for &digit in digits.get(..kept)? {
        integer = integer.checked_mul(10)?.checked_add(u64::from(digit))?;
    }
    // The discarded part: its first digit (0 when every digit is discarded with room to spare)
    // and whether anything follows it.
    let (first, rest) = if kept as i64 == exponent.saturating_add(precision).saturating_add(1) {
        (
            digits.get(kept).copied().unwrap_or(0),
            digits
                .get(kept + 1..)
                .is_some_and(|r| r.iter().any(|&d| d != 0)),
        )
    } else {
        (0, digits.iter().any(|&d| d != 0))
    };
    let discarded = first != 0 || rest;
    let half = match first.cmp(&5) {
        std::cmp::Ordering::Less => std::cmp::Ordering::Less,
        std::cmp::Ordering::Greater => std::cmp::Ordering::Greater,
        std::cmp::Ordering::Equal if rest => std::cmp::Ordering::Greater,
        std::cmp::Ordering::Equal => std::cmp::Ordering::Equal,
    };
    let negative = x < 0.0;
    let increment = match mode {
        RoundingMode::Up => discarded,
        RoundingMode::Down => false,
        RoundingMode::Ceiling => discarded && !negative,
        RoundingMode::Floor => discarded && negative,
        RoundingMode::HalfUp => half != std::cmp::Ordering::Less,
        RoundingMode::HalfDown => half == std::cmp::Ordering::Greater,
        RoundingMode::HalfEven => {
            half == std::cmp::Ordering::Greater
                || (half == std::cmp::Ordering::Equal && integer % 2 == 1)
        }
    };
    let integer = integer.checked_add(u64::from(increment))?;
    // `precision` < count - exponent - 1 <= 17 + 324 here, so the text stays short; parsing it
    // gives the double nearest to the decimal (Java `BigDecimal.doubleValue`).
    let mut result = Text::new();
    let sign = if negative { "-" } else { "" };
    write!(result, "{sign}{integer}e-{precision}").ok()?;
    std::str::from_utf8(result.as_bytes()).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::super::super::eval::status;
    use super::super::test_env::*;
    use super::*;

    #[test]
    fn abs_floor_ceil_keep_the_type() {
        check_values(&[
            ("abs(-3)", int(3)),
            ("abs(3)", int(3)),
            ("abs(-2.5)", float(2.5)),
            ("abs($big)", int(i64::MAX)),
            ("floor(3)", int(3)),
            ("floor(-3)", int(-3)),
            ("floor(2.7)", float(2.0)),
            ("floor(-2.2)", float(-3.0)),
            ("ceil(2.2)", float(3.0)),
            ("ceiling(-2.7)", float(-2.0)),
            ("ceil(7)", int(7)),
            ("ceil($min)", int(i64::MIN)),
            ("abs(NULL)", null()),
            ("floor(x)", null()),
        ]);
        check_statuses(&[
            ("abs($min)", status::OUT_OF_RANGE),
            ("abs('3')", status::INVALID_VALUE_TYPE),
            ("floor(TRUE)", status::INVALID_VALUE_TYPE),
            ("ceil([1])", status::INVALID_VALUE_TYPE),
        ]);
        let error = eval("abs('3')").unwrap_err();
        assert_eq!(error.message, "abs() takes a number, not a string");
    }

    #[test]
    fn sign_is_an_integer() {
        check_values(&[
            ("sign(-7)", int(-1)),
            ("sign(0)", int(0)),
            ("sign(12)", int(1)),
            ("sign(-0.5)", int(-1)),
            ("sign(0.0)", int(0)),
            ("sign(-0.0)", int(0)),
            ("sign(2.5)", int(1)),
            ("sign($min)", int(-1)),
            ("sign(NULL)", null()),
        ]);
        assert_eq!(status_of("sign('a')"), status::INVALID_VALUE_TYPE);
    }

    #[test]
    fn round_with_one_argument_is_java_math_round() {
        check_values(&[
            ("round(2.5)", float(3.0)),
            ("round(-2.5)", float(-2.0)),
            ("round(-1.5)", float(-1.0)),
            ("round(0.5)", float(1.0)),
            ("round(-0.5)", float(0.0)),
            ("round(2.4)", float(2.0)),
            ("round(-2.6)", float(-3.0)),
            // The float just below 0.5 rounds down (Java 7+).
            ("round(0.49999999999999994)", float(0.0)),
            ("round(7)", float(7.0)),
            ("round($big)", float(i64::MAX as f64)),
            ("round(1.0e20)", float(1.0e20)),
            // Precision 0 without a mode is the one-argument form.
            ("round(-2.5, 0)", float(-2.0)),
            ("round(-1.5, 0)", float(-1.0)),
            ("round(2.5, 0)", float(3.0)),
            ("round(NULL)", null()),
            ("round(2.5, NULL)", null()),
            ("round(2.5, 1, NULL)", null()),
            ("round(NULL, 1, 'NOPE')", null()),
        ]);
    }

    #[test]
    fn round_with_a_precision_rounds_the_decimal_text() {
        check_values(&[
            // The shortest text is 2.675, so the tie rounds up (not 2.67 as on the binary value).
            ("round(2.675, 2)", float(2.68)),
            ("round(-1.55, 1)", float(-1.6)),
            ("round(1.005, 2)", float(1.01)),
            ("round(1.23456, 3)", float(1.235)),
            ("round(9.99, 1)", float(10.0)),
            ("round(-9.96, 1)", float(-10.0)),
            ("round(0.0004, 2)", float(0.0)),
            ("round(1.25, 5)", float(1.25)),
            ("round(1234, 2)", float(1234.0)),
            ("round(1.5, $big)", float(1.5)),
            ("round(0.000123456, 5)", float(0.00012)),
            ("round(1.0e-300, 2)", float(0.0)),
            ("round(1.0e20, 3)", float(1.0e20)),
            ("round(0.0, 3)", float(0.0)),
            // An explicit mode at precision 0 is decimal rounding too.
            ("round(-2.5, 0, 'HALF_UP')", float(-3.0)),
            ("round(2.5, 0, 'HALF_EVEN')", float(2.0)),
            ("round(0.0004, 2, 'UP')", float(0.01)),
            ("round(-0.0004, 2, 'UP')", float(-0.01)),
            ("round(-0.0004, 2, 'CEILING')", float(0.0)),
            ("round(-0.0004, 2, 'FLOOR')", float(-0.01)),
        ]);
    }

    #[test]
    fn every_mode_on_ties_and_non_ties() {
        // (mode, round(x.25, 1) for x.25 = 2.25, -2.25, 2.35, -2.35, 2.26, -2.24)
        let table: &[(&str, [f64; 6])] = &[
            ("UP", [2.3, -2.3, 2.4, -2.4, 2.3, -2.3]),
            ("DOWN", [2.2, -2.2, 2.3, -2.3, 2.2, -2.2]),
            ("CEILING", [2.3, -2.2, 2.4, -2.3, 2.3, -2.2]),
            ("FLOOR", [2.2, -2.3, 2.3, -2.4, 2.2, -2.3]),
            ("HALF_UP", [2.3, -2.3, 2.4, -2.4, 2.3, -2.2]),
            ("HALF_DOWN", [2.2, -2.2, 2.3, -2.3, 2.3, -2.2]),
            ("HALF_EVEN", [2.2, -2.2, 2.4, -2.4, 2.3, -2.2]),
        ];
        let inputs = ["2.25", "-2.25", "2.35", "-2.35", "2.26", "-2.24"];
        for (mode, expected) in table {
            for (input, expected) in inputs.iter().zip(expected) {
                let expr = format!("round({input}, 1, '{mode}')");
                assert_eq!(value(&expr), float(*expected), "{expr}");
            }
        }
        // x.5 at precision 0, every mode.
        let table: &[(&str, [f64; 4])] = &[
            ("UP", [3.0, -3.0, 4.0, -4.0]),
            ("DOWN", [2.0, -2.0, 3.0, -3.0]),
            ("CEILING", [3.0, -2.0, 4.0, -3.0]),
            ("FLOOR", [2.0, -3.0, 3.0, -4.0]),
            ("HALF_UP", [3.0, -3.0, 4.0, -4.0]),
            ("HALF_DOWN", [2.0, -2.0, 3.0, -3.0]),
            ("HALF_EVEN", [2.0, -2.0, 4.0, -4.0]),
        ];
        let inputs = ["2.5", "-2.5", "3.5", "-3.5"];
        for (mode, expected) in table {
            for (input, expected) in inputs.iter().zip(expected) {
                let expr = format!("round({input}, 0, '{mode}')");
                assert_eq!(value(&expr), float(*expected), "{expr}");
            }
        }
    }

    #[test]
    fn round_refuses_bad_precisions_and_modes() {
        check_statuses(&[
            ("round(1234, -2)", status::INVALID_PARAMETER),
            ("round(1.5, -1, 'UP')", status::INVALID_PARAMETER),
            ("round(1.5, 1, 'half_up')", status::INVALID_PARAMETER),
            ("round(1.5, 1, 'UNNECESSARY')", status::INVALID_PARAMETER),
            ("round(1.5, 1, '')", status::INVALID_PARAMETER),
            ("round(1.5, 1.0)", status::INVALID_VALUE_TYPE),
            ("round(1.5, '1')", status::INVALID_VALUE_TYPE),
            ("round(1.5, 1, 1)", status::INVALID_VALUE_TYPE),
            ("round('1.5')", status::INVALID_VALUE_TYPE),
        ]);
        let error = eval("round(1.5, 1, 'half_up')").unwrap_err();
        assert!(error.message.contains("'HALF_UP'"), "{}", error.message);
        assert!(error.message.contains("upper case"), "{}", error.message);
        let error = eval("round(1.5, 1, 'UNNECESSARY')").unwrap_err();
        assert!(error.message.contains("'UNNECESSARY'"), "{}", error.message);
        let error = eval("round(1234, -2)").unwrap_err();
        assert_eq!(error.message, "round() needs a precision >= 0, not -2");
    }

    #[test]
    fn roots_exponentials_and_logarithms_are_floats() {
        check_values(&[
            ("sqrt(16)", float(4.0)),
            ("sqrt(2.25)", float(1.5)),
            ("sqrt(0)", float(0.0)),
            ("exp(0)", float(1.0)),
            ("exp(-1000)", float(0.0)),
            ("ln(1)", float(0.0)),
            ("log(1)", float(0.0)),
            ("log10(1000)", float(3.0)),
            ("log10(0.01)", float(-2.0)),
            ("log(2, 8)", float(3.0)),
            ("log(10, 0.001)", float(-3.0)),
            ("log(0.5, 4)", float(-2.0)),
            ("power(2, 10)", float(1024.0)),
            ("power(2, -1)", float(0.5)),
            ("power(-8, 3)", float(-512.0)),
            ("power(-2, 2.0)", float(4.0)),
            ("power(9, 0.5)", float(3.0)),
            ("power(0, 0)", float(1.0)),
            ("power(0, 2)", float(0.0)),
            ("sqrt(NULL)", null()),
            ("log(NULL, 2)", null()),
            ("log(2, NULL)", null()),
            ("power(NULL, -1)", null()),
        ]);
        assert_eq!(value("exp(1)"), float(1f64.exp()));
        assert_eq!(value("ln(10)"), float(10f64.ln()));
        assert_eq!(value("log(3, 10)"), float(10f64.ln() / 3f64.ln()));
        check_statuses(&[
            ("sqrt(-1)", status::POWER),
            ("sqrt(-0.5)", status::POWER),
            ("exp(1000)", status::OUT_OF_RANGE),
            ("ln(0)", status::LOGARITHM),
            ("ln(-1)", status::LOGARITHM),
            ("log(0)", status::LOGARITHM),
            ("log10(0)", status::LOGARITHM),
            ("log10(-5.5)", status::LOGARITHM),
            ("log(1, 5)", status::LOGARITHM),
            ("log(0, 5)", status::LOGARITHM),
            ("log(-2, 5)", status::LOGARITHM),
            ("log(2, 0)", status::LOGARITHM),
            ("log(2, -1)", status::LOGARITHM),
            ("power(0, -1)", status::POWER),
            ("power(0.0, -0.5)", status::POWER),
            ("power(-8, 1.0 / 3)", status::POWER),
            ("power(-2, 0.5)", status::POWER),
            ("power(10, 400)", status::OUT_OF_RANGE),
            ("power($big, 50)", status::OUT_OF_RANGE),
            ("sqrt('4')", status::INVALID_VALUE_TYPE),
            ("power(2, 'x')", status::INVALID_VALUE_TYPE),
            ("log(2, TRUE)", status::INVALID_VALUE_TYPE),
        ]);
        let error = eval("log(1, 5)").unwrap_err();
        assert_eq!(
            error.message,
            "log(base, x) needs a base > 0 and <> 1, not 1"
        );
    }

    #[test]
    fn mod_is_the_remainder_operator() {
        check_values(&[
            ("mod(7, 3)", int(1)),
            ("mod(-7, 3)", int(-1)),
            ("mod(7, -3)", int(1)),
            ("mod($min, -1)", int(0)),
            ("mod(7.5, 2)", float(1.5)),
            ("mod(-7.5, 2)", float(-1.5)),
            ("mod(7, 2.5)", float(2.0)),
            ("mod(NULL, 0)", null()),
        ]);
        check_statuses(&[
            ("mod(7, 0)", status::DIVISION_BY_ZERO),
            ("mod(7.5, 0)", status::DIVISION_BY_ZERO),
            ("mod(7, 0.0)", status::DIVISION_BY_ZERO),
            ("mod('7', 2)", status::INVALID_VALUE_TYPE),
        ]);
        for (a, b) in [(7, 3), (-7, 3), (7, -3), (0, 5)] {
            assert_eq!(
                value(&format!("mod({a}, {b})")),
                value(&format!("{a} % {b}"))
            );
        }
    }

    #[test]
    fn errors_point_at_the_offending_argument() {
        let error = eval("log(2, -1)").unwrap_err();
        let source = "MATCH (n) RETURN log(2, -1)";
        assert_eq!(&source[error.span.start..error.span.end], "-1");
        let error = eval("round(1.5, 1, 'x')").unwrap_err();
        let source = "MATCH (n) RETURN round(1.5, 1, 'x')";
        assert_eq!(&source[error.span.start..error.span.end], "'x'");
        let error = eval("mod(1, 0)").unwrap_err();
        let source = "MATCH (n) RETURN mod(1, 0)";
        assert_eq!(&source[error.span.start..error.span.end], "0");
    }

    const SPAN: Span = Span {
        start: 0,
        end: 0,
        line: 1,
        column: 1,
    };

    fn call_on<const K: usize>(
        function: Function,
        values: [Value<El>; K],
    ) -> Result<Value<El>, EvalError> {
        let args = values
            .into_iter()
            .map(|value| Arg { value, span: SPAN })
            .collect();
        call(function, args, &TestRow::default(), SPAN)
    }

    #[test]
    fn stored_nan_and_infinities_pass_through() {
        let nan = float(f64::NAN);
        let inf = float(f64::INFINITY);
        let neg_inf = float(f64::NEG_INFINITY);
        for function in [
            Function::Abs,
            Function::Floor,
            Function::Ceil,
            Function::Round,
        ] {
            assert_eq!(call_on(function, [nan.clone()]), Ok(nan.clone()));
            assert_eq!(call_on(function, [inf.clone()]), Ok(inf.clone()));
        }
        assert_eq!(call_on(Function::Abs, [neg_inf.clone()]), Ok(inf.clone()));
        assert_eq!(
            call_on(Function::Round, [nan.clone(), int(2), string("UP")]),
            Ok(nan.clone())
        );
        assert_eq!(
            call_on(Function::Round, [neg_inf.clone(), int(2)]),
            Ok(neg_inf.clone())
        );
        assert_eq!(call_on(Function::Sign, [nan.clone()]), Ok(int(0)));
        assert_eq!(call_on(Function::Sign, [neg_inf.clone()]), Ok(int(-1)));
        assert_eq!(call_on(Function::Sqrt, [nan.clone()]), Ok(nan.clone()));
        assert_eq!(call_on(Function::Exp, [inf.clone()]), Ok(inf.clone()));
        assert_eq!(call_on(Function::Ln, [inf.clone()]), Ok(inf.clone()));
        assert_eq!(
            call_on(Function::Power, [nan.clone(), int(2)]),
            Ok(nan.clone())
        );
        // Invalid arguments are still refused, whatever the other value is.
        assert_eq!(
            call_on(Function::Round, [nan.clone(), int(-1)])
                .unwrap_err()
                .gqlstatus,
            status::INVALID_PARAMETER
        );
    }

    #[test]
    fn other_functions_are_not_numeric() {
        let error = call_on(Function::Left, [string("a"), int(1)]).unwrap_err();
        assert_eq!(error.gqlstatus, status::NOT_SUPPORTED);
    }

    #[test]
    fn decimal_round_agrees_with_a_reference_on_many_values() {
        // Reference: exact decimal arithmetic on the shortest text, half away from zero, then
        // parsed back. Values with up to 4 decimals at precision 0..=3.
        for numerator in -20_000i64..=20_000 {
            if numerator % 7 != 0 {
                continue;
            }
            let x = numerator as f64 / 10_000.0;
            for precision in 0..=3i64 {
                let scale = 10i64.pow((4 - precision) as u32);
                let (q, r) = (numerator / scale, (numerator % scale).abs());
                let up = r * 2 >= scale;
                let rounded = if up { q + numerator.signum() } else { q };
                let expected: f64 = format!("{rounded}e-{precision}")
                    .parse()
                    .unwrap_or(f64::NAN);
                assert_eq!(
                    decimal_round(x, precision, RoundingMode::HalfUp),
                    Some(expected),
                    "round({x}, {precision})"
                );
            }
        }
    }
}
