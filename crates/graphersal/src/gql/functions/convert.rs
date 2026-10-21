//! Conversions (design K-D22, F10): GQL `CAST(x AS INTEGER | FLOAT | STRING |
//! BOOLEAN)` (strict), Cypher `toInteger` / `toFloat` / `toBoolean` / `toString` (lenient) and
//! their `...OrNull` forms, one core with a [`Mode`].
//!
//! The contract of a family file is in the module docs of `functions/mod.rs`; unit tests use
//! the test `Env` and helpers of `functions/test_env.rs`.
//!
//! # Rules
//!
//! `NULL` converts to `NULL` in every mode (design F2). The outcome of a conversion is a value or
//! one of three failures, which the [`Mode`] maps:
//!
//! | Failure | `CAST` | `toX` | `toXOrNull` |
//! |---|---|---|---|
//! | a string that is no text of the target (`'abc'` to INTEGER) | 22018 | `NULL` | `NULL` |
//! | a value of the wrong kind (22G03) or out of range (22003) | error | error | `NULL` |
//! | a list or a map to STRING (22G03) | text | error | `NULL` |
//! | an element or a path to STRING (22G03) | error | error | `NULL` |
//! | a refused result size (54001) | error | error | error |
//!
//! Texts per target (`trim` below: `CAST` strips the space U+0020 only, `toX` strips the
//! characters <= U+0020 as Java `String.trim`, `toBoolean` the Java whitespace):
//!
//! - **INTEGER**: an integer stays; a float is truncated toward zero (NaN, infinite or outside
//!   `i64`: 22003); a string is trimmed, then a decimal literal (`'42'`, `'-1.9'` -> -1,
//!   `'1e3'` -> 1000; Java `BigDecimal` grammar, truncated, 22003 outside `i64`); `toInteger`
//!   then also accepts a Cypher number literal (`'0x1F'`, `'0o17'`, `'1_000'`). A boolean is
//!   1 / 0 for `toInteger`, 22G03 for `CAST`.
//! - **FLOAT**: a number becomes a float; a string is trimmed, then a decimal literal, `'NaN'`,
//!   `'Infinity'`, `'-Infinity'` (exact case, Java), and for `toFloat` a Cypher number literal;
//!   a literal beyond the float range is 22003 (Neo4j: Infinity). A boolean is 22G03.
//! - **BOOLEAN**: a boolean stays; a string is trimmed, then `true` / `false` in any ASCII case,
//!   and for `CAST` also `unknown` (-> `NULL`); an integer is `0` -> FALSE, else TRUE for
//!   `toBoolean`, 22G03 for `CAST`; a float is 22G03.
//! - **STRING**: a string stays; an integer, a uuid, a vector: the engine text
//!   (`ElementProperty::cast`); a temporal: for `CAST` the engine text (ISO 8601, `12:00:00`,
//!   `-PT1M0.001S`), for `toString` / `toStringOrNull` the Neo4j / `java.time` text (stage 3g,
//!   user decision U6: `12:00`, `1984-10-11T12:31Z`, `PT-1M-0.001S`,
//!   `temporal::ops::to_text`); a float: the Java `Double.toString` form (design F10: `"1.0"`,
//!   `"1.5E20"`, `"1.0E-4"`, `"NaN"`, `"Infinity"`); a boolean: `"true"` / `"false"`; a list or a
//!   map: for `CAST` the Neo4j text form (`"[1, a, true]"`, `"{a: 1, b: hi}"`, strings
//!   unquoted, `NULL` as `null`), its size reserved first ([`Env::reserve`], F6); for
//!   `toString` 22G03 and for `toStringOrNull` `NULL` (stage 3f, user decision U11: the
//!   openCypher TCK `TypeConversion4`); an element or a path, also inside a list or a map:
//!   22G03 (`toStringOrNull`: `NULL` for the element or path itself), naming `element_id(x)` /
//!   `properties(x)` / `RETURN p`.

use super::super::ast::CastTarget;
use super::super::eval::{Env, EvalError, Value, status};
use super::super::lexer::Span;
use super::{Arg, Conversion, Mode, reserve};
use crate::ElementProperty;
use crate::schema::ValueType;
use crate::value::number::N;

/// The deepest list / map nesting `toString` renders (recursion bound).
const MAX_TEXT_DEPTH: usize = 1024;

/// The most characters of a string quoted in an error message.
const QUOTED_CHARS: usize = 40;

/// Why a conversion failed (see the module docs for what each [`Mode`] makes of it).
enum Failure {
    /// A string that is no text of the target: 22018 for `CAST`, `NULL` otherwise.
    Unparsable,
    /// A wrong kind or a value out of range: `NULL` for `...OrNull`, else the error.
    Failed(EvalError),
    /// An error in every mode.
    Fatal(EvalError),
}

/// Converts `arg` as `conversion` says (the target and the [`Mode`]); `span` is the whole call.
pub(crate) fn convert<En: Env>(
    conversion: Conversion,
    arg: Arg<En::Element>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    if arg.is_null() {
        return Ok(Value::Null);
    }
    let result = match conversion.target {
        CastTarget::Integer => to_integer(conversion, &arg),
        CastTarget::Float => to_float(conversion, &arg),
        CastTarget::Boolean => to_boolean(conversion, &arg),
        CastTarget::String => to_string(conversion, &arg, env, span),
    };
    match (result, conversion.mode) {
        (Ok(value), _) => Ok(value),
        (Err(Failure::Fatal(error)), _) => Err(error),
        (Err(Failure::Unparsable | Failure::Failed(_)), Mode::OrNull)
        | (Err(Failure::Unparsable), Mode::Lenient) => Ok(Value::Null),
        (Err(Failure::Failed(error)), Mode::Cast | Mode::Lenient) => Err(error),
        (Err(Failure::Unparsable), Mode::Cast) => Err(invalid_cast(conversion, &arg)),
    }
}

// ----- INTEGER -----

fn to_integer<E>(conversion: Conversion, arg: &Arg<E>) -> Result<Value<E>, Failure> {
    let property = match &arg.value {
        Value::Scalar(property) => property,
        Value::Null => return Ok(Value::Null),
        Value::List(_) | Value::Map(_) | Value::Element(_) | Value::Path(_) => {
            return Err(wrong_kind(conversion, "a number or a string", arg));
        }
    };
    match property {
        ElementProperty::Number(number) => match number.n {
            N::NegInt(value) => Ok(Value::integer(value)),
            N::Float(value) => truncate_float(value.0)
                .map(Value::integer)
                .ok_or_else(|| out_of_range(conversion, arg)),
        },
        ElementProperty::String(text) => parse_integer(conversion, text, arg),
        ElementProperty::Boolean(flag) => match conversion.mode {
            Mode::Cast => Err(wrong_kind(conversion, "a number or a string", arg)),
            Mode::Lenient | Mode::OrNull => Ok(Value::integer(i64::from(*flag))),
        },
        ElementProperty::Null => Ok(Value::Null),
        ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        | ElementProperty::Vector(_)
        | ElementProperty::Object(_)
        | ElementProperty::Array(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_)
        | ElementProperty::Map(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => Err(wrong_kind(conversion, "a number or a string", arg)),
    }
}

/// `value` truncated toward zero, `None` when NaN, infinite or outside `i64`.
fn truncate_float(value: f64) -> Option<i64> {
    let truncated = value.trunc();
    // -2^63 is exact as a float; 2^63 is the first value above `i64::MAX`.
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&truncated) {
        Some(truncated as i64)
    } else {
        None
    }
}

fn parse_integer<E>(conversion: Conversion, text: &str, arg: &Arg<E>) -> Result<Value<E>, Failure> {
    let lenient = conversion.mode != Mode::Cast;
    let trimmed = trim_for(conversion, text);
    let parsed = match Decimal::parse(trimmed) {
        Some(decimal) => Some(decimal.truncate()),
        None if lenient => cypher_literal(trimmed).map(|literal| literal.integer),
        None => None,
    };
    match parsed {
        Some(Some(value)) => Ok(Value::integer(value)),
        Some(None) => Err(out_of_range(conversion, arg)),
        None => Err(Failure::Unparsable),
    }
}

// ----- FLOAT -----

fn to_float<E>(conversion: Conversion, arg: &Arg<E>) -> Result<Value<E>, Failure> {
    let property = match &arg.value {
        Value::Scalar(property) => property,
        Value::Null => return Ok(Value::Null),
        Value::List(_) | Value::Map(_) | Value::Element(_) | Value::Path(_) => {
            return Err(wrong_kind(conversion, "a number or a string", arg));
        }
    };
    match property {
        ElementProperty::Number(number) => Ok(Value::float(match number.n {
            N::NegInt(value) => value as f64,
            N::Float(value) => value.0,
        })),
        ElementProperty::String(text) => parse_float(conversion, text, arg),
        ElementProperty::Null => Ok(Value::Null),
        ElementProperty::Boolean(_)
        | ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        | ElementProperty::Vector(_)
        | ElementProperty::Object(_)
        | ElementProperty::Array(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_)
        | ElementProperty::Map(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => Err(wrong_kind(conversion, "a number or a string", arg)),
    }
}

fn parse_float<E>(conversion: Conversion, text: &str, arg: &Arg<E>) -> Result<Value<E>, Failure> {
    let trimmed = trim_for(conversion, text);
    let special = match trimmed {
        "NaN" => Some(f64::NAN),
        "Infinity" | "+Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        _ => None,
    };
    if let Some(value) = special {
        return Ok(Value::float(value));
    }
    let parsed = match Decimal::parse(trimmed) {
        Some(decimal) => Some(decimal.to_float()),
        None if conversion.mode != Mode::Cast => cypher_literal(trimmed).map(|l| l.float),
        None => None,
    };
    match parsed {
        Some(Some(value)) => Ok(Value::float(value)),
        Some(None) => Err(out_of_range(conversion, arg)),
        None => Err(Failure::Unparsable),
    }
}

// ----- BOOLEAN -----

fn to_boolean<E>(conversion: Conversion, arg: &Arg<E>) -> Result<Value<E>, Failure> {
    let what = match conversion.mode {
        Mode::Cast => "a boolean or a string",
        Mode::Lenient | Mode::OrNull => "a boolean, a string or an integer",
    };
    let property = match &arg.value {
        Value::Scalar(property) => property,
        Value::Null => return Ok(Value::Null),
        Value::List(_) | Value::Map(_) | Value::Element(_) | Value::Path(_) => {
            return Err(wrong_kind(conversion, what, arg));
        }
    };
    match property {
        ElementProperty::Boolean(flag) => Ok(Value::boolean(*flag)),
        ElementProperty::String(text) => {
            let trimmed = match conversion.mode {
                Mode::Cast => text.trim_matches(' '),
                Mode::Lenient | Mode::OrNull => text.trim_matches(java_whitespace),
            };
            if trimmed.eq_ignore_ascii_case("true") {
                Ok(Value::boolean(true))
            } else if trimmed.eq_ignore_ascii_case("false") {
                Ok(Value::boolean(false))
            } else if conversion.mode == Mode::Cast && trimmed.eq_ignore_ascii_case("unknown") {
                Ok(Value::Null)
            } else {
                Err(Failure::Unparsable)
            }
        }
        ElementProperty::Number(number) => match (number.n.clone(), conversion.mode) {
            (N::NegInt(value), Mode::Lenient | Mode::OrNull) => Ok(Value::boolean(value != 0)),
            (N::NegInt(_), Mode::Cast) | (N::Float(_), _) => Err(wrong_kind(conversion, what, arg)),
        },
        ElementProperty::Null => Ok(Value::Null),
        ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        | ElementProperty::Vector(_)
        | ElementProperty::Object(_)
        | ElementProperty::Array(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_)
        | ElementProperty::Map(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => Err(wrong_kind(conversion, what, arg)),
    }
}

/// Java `Character.isWhitespace`: the Unicode space, line and paragraph separators except the
/// non-breaking ones (U+00A0, U+2007, U+202F), and U+0009-U+000D, U+001C-U+001F.
fn java_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{D}'
            | '\u{1C}'..='\u{20}'
            | '\u{1680}'
            | '\u{2000}'..='\u{2006}'
            | '\u{2008}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

/// The text a number conversion parses: `CAST` strips the space, `toX` the characters
/// <= U+0020 (Java `String.trim`).
fn trim_for(conversion: Conversion, text: &str) -> &str {
    match conversion.mode {
        Mode::Cast => text.trim_matches(' '),
        Mode::Lenient | Mode::OrNull => text.trim_matches(|c: char| c <= ' '),
    }
}

// ----- number texts -----

/// A decimal literal (Java `BigDecimal` grammar): `[+-] (digits [. [digits]] | . digits)
/// [(e|E) [+-] digits]`, ASCII only.
struct Decimal<'a> {
    negative: bool,
    /// The digits before the point.
    whole: &'a str,
    /// The digits after the point.
    fraction: &'a str,
    /// The exponent, saturated (only its order of magnitude matters).
    exponent: i64,
    /// The validated text (for the float parser).
    text: &'a str,
}

impl<'a> Decimal<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let (negative, body) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = match body.find(['e', 'E']) {
            Some(at) => (&body[..at], Some(&body[at + 1..])),
            None => (body, None),
        };
        let (whole, fraction) = match mantissa.find('.') {
            Some(at) => (&mantissa[..at], &mantissa[at + 1..]),
            None => (mantissa, ""),
        };
        let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
        if (whole.is_empty() && fraction.is_empty()) || !digits(whole) || !digits(fraction) {
            return None;
        }
        let exponent = match exponent {
            None => 0,
            Some(exponent) => {
                let (sign, digits_part) = match exponent.as_bytes().first() {
                    Some(b'-') => (-1, &exponent[1..]),
                    Some(b'+') => (1, &exponent[1..]),
                    _ => (1, exponent),
                };
                if digits_part.is_empty() || !digits(digits_part) {
                    return None;
                }
                let magnitude = digits_part.bytes().fold(0i64, |acc, b| {
                    acc.saturating_mul(10).saturating_add(i64::from(b - b'0'))
                });
                sign * magnitude
            }
        };
        Some(Decimal {
            negative,
            whole,
            fraction,
            exponent,
            text,
        })
    }

    /// The value truncated toward zero; `None` outside `i64`.
    fn truncate(&self) -> Option<i64> {
        let digits = || self.whole.bytes().chain(self.fraction.bytes());
        let zeros = digits().take_while(|b| *b == b'0').count();
        if zeros == self.whole.len() + self.fraction.len() {
            return Some(0);
        }
        // value = 0.<significant digits> * 10^point
        let point = self.whole.len() as i128 + i128::from(self.exponent) - zeros as i128;
        if point <= 0 {
            return Some(0);
        }
        // `i64::MAX` has 19 digits.
        if point > 19 {
            return None;
        }
        let mut significant = digits().skip(zeros);
        let mut magnitude: u128 = 0;
        for _ in 0..point {
            let digit = significant.next().map_or(0, |b| b - b'0');
            magnitude = magnitude * 10 + u128::from(digit);
        }
        signed(self.negative, magnitude)
    }

    /// The nearest float; `None` beyond the float range.
    fn to_float(&self) -> Option<f64> {
        let value: f64 = self.text.parse().ok()?;
        value.is_finite().then_some(value)
    }
}

/// A Cypher number literal read as INTEGER and as FLOAT (`None`: out of range).
struct Literal {
    integer: Option<i64>,
    float: Option<f64>,
}

/// A Cypher number literal (the `toX` fallback, P0): `[+-]` then `0x` hexadecimal or `0o`
/// octal digits, or a decimal literal whose digits are grouped by `_` (`1_000`); `None` for
/// any other text (a plain decimal literal is read before, by [`Decimal`]).
fn cypher_literal(text: &str) -> Option<Literal> {
    let (negative, body) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let radix = match body.get(..2) {
        Some("0x") => 16,
        Some("0o") => 8,
        _ => {
            if !text.contains('_') {
                return None;
            }
            let plain = without_underscores(text, |c| c.is_ascii_digit())?;
            let decimal = Decimal::parse(&plain)?;
            return Some(Literal {
                integer: decimal.truncate(),
                float: decimal.to_float(),
            });
        }
    };
    let digits = without_underscores(&body[2..], |c| c.is_digit(radix))?;
    if digits.is_empty() {
        return None;
    }
    let mut magnitude: u128 = 0;
    for c in digits.chars() {
        let digit = c.to_digit(radix)?;
        // Saturate past i64 (out of range anyway); u128 never overflows here.
        magnitude = (magnitude * u128::from(radix) + u128::from(digit)).min(1u128 << 64);
    }
    let integer = signed(negative, magnitude);
    Some(Literal {
        integer,
        float: integer.map(|value| value as f64),
    })
}

/// `magnitude` with its sign as an `i64`, `None` outside the range.
fn signed(negative: bool, magnitude: u128) -> Option<i64> {
    if negative {
        let value = -i128::try_from(magnitude).ok()?;
        i64::try_from(value).ok()
    } else {
        i64::try_from(magnitude).ok()
    }
}

/// The text without the `_` that stand between two digits (`is_digit`); `None` when an `_`
/// stands elsewhere (first, last, next to a non-digit).
fn without_underscores(text: &str, is_digit: impl Fn(char) -> bool) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut previous: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            let after = chars
                .peek()
                .is_some_and(|next| *next == '_' || is_digit(*next));
            if !previous.is_some_and(&is_digit) || !after {
                return None;
            }
            // `previous` stays the digit before, so `1__000` reads like `1_000`.
            continue;
        }
        out.push(c);
        previous = Some(c);
    }
    Some(out)
}

// ----- STRING -----

fn to_string<En: Env>(
    conversion: Conversion,
    arg: &Arg<En::Element>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, Failure> {
    if let Some(text) = arg.value.as_str() {
        return Ok(Value::string(text));
    }
    // Cypher `toString` takes scalars only (stage 3f, the openCypher TCK): a list or a map is
    // 22G03 (`toStringOrNull`: `NULL`), an element or a path too; `CAST` writes lists and maps.
    if conversion.mode != Mode::Cast {
        match &arg.value {
            Value::List(_) | Value::Map(_) => {
                return Err(wrong_kind(
                    conversion,
                    "a number, a boolean, a string or another scalar (CAST(x AS STRING) writes \
                     a list or a map as text)",
                    arg,
                ));
            }
            Value::Element(_) | Value::Path(_) if conversion.mode == Mode::OrNull => {
                return Ok(Value::Null);
            }
            Value::Null | Value::Scalar(_) | Value::Element(_) | Value::Path(_) => {}
        }
    }
    // First the size (nothing built), then the reservation (design F6), then the text.
    let mut size = Counter(0);
    write_text(conversion, &arg.value, arg, &mut size, 0)?;
    reserve(env, size.0, span).map_err(Failure::Fatal)?;
    let mut text = String::with_capacity(usize::try_from(size.0).unwrap_or(0));
    write_text(conversion, &arg.value, arg, &mut text, 0)?;
    Ok(Value::string(text))
}

/// Where a text goes: counted or built.
trait Sink {
    fn push(&mut self, text: &str);
}

/// Counts the bytes of a text without building it.
struct Counter(u64);

impl Sink for Counter {
    fn push(&mut self, text: &str) {
        self.0 = self.0.saturating_add(text.len() as u64);
    }
}

impl Sink for String {
    fn push(&mut self, text: &str) {
        self.push_str(text);
    }
}

/// Writes the text of `value` (the module docs: Neo4j's form for lists and maps); `arg` is the
/// argument the errors point at.
fn write_text<E>(
    conversion: Conversion,
    value: &Value<E>,
    arg: &Arg<E>,
    out: &mut impl Sink,
    depth: usize,
) -> Result<(), Failure> {
    if depth > MAX_TEXT_DEPTH {
        return Err(Failure::Fatal(EvalError::new(
            status::LIMIT,
            format!(
                "{} cannot render a value nested more than {MAX_TEXT_DEPTH} levels deep",
                conversion.name()
            ),
            arg.span,
        )));
    }
    match value {
        Value::Null => out.push("null"),
        Value::Scalar(property) => write_scalar(conversion, property, arg, out, depth)?,
        Value::List(items) => {
            out.push("[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(", ");
                }
                write_text(conversion, item, arg, out, depth + 1)?;
            }
            out.push("]");
        }
        Value::Map(entries) => {
            out.push("{");
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(", ");
                }
                out.push(key);
                out.push(": ");
                write_text(conversion, item, arg, out, depth + 1)?;
            }
            out.push("}");
        }
        Value::Element(_) | Value::Path(_) => return Err(element_to_string(conversion, arg)),
    }
    Ok(())
}

fn write_scalar<E>(
    conversion: Conversion,
    property: &ElementProperty,
    arg: &Arg<E>,
    out: &mut impl Sink,
    depth: usize,
) -> Result<(), Failure> {
    match property {
        ElementProperty::Null => out.push("null"),
        ElementProperty::String(text) => out.push(text),
        ElementProperty::Boolean(flag) => out.push(if *flag { "true" } else { "false" }),
        ElementProperty::Number(number) => match number.n {
            N::NegInt(value) => out.push(&value.to_string()),
            N::Float(value) => out.push(&float_text(value.0)),
        },
        // Cypher `toString` writes the Neo4j text of a temporal (stage 3g, U6: `12:00`,
        // `PT-1M-0.001S`); `CAST` keeps the engine text below, the shared `cast` is unchanged.
        ElementProperty::Temporal(value) if conversion.mode != Mode::Cast => {
            out.push(&super::temporal::ops::to_text(value))
        }
        ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        | ElementProperty::Vector(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => match property.cast(&ValueType::String) {
            Ok(ElementProperty::String(text)) => out.push(&text),
            _ => return Err(wrong_kind(conversion, "a convertible value", arg)),
        },
        // Never a scalar of a GQL value (`Value::from_property` makes lists and maps of
        // them); read the same way if one appears.
        ElementProperty::Object(_) | ElementProperty::Array(_) => {
            let nested = Value::<E>::from_property(property.clone());
            write_text(conversion, &nested, arg, out, depth + 1)?;
        }
        ElementProperty::Vertex(_) | ElementProperty::Edge(_) => {
            return Err(element_to_string(conversion, arg));
        }
        // A map with a non-string key has no GQL text.
        ElementProperty::Map(_) => {
            return Err(wrong_kind(conversion, "a value with a text form", arg));
        }
    }
    Ok(())
}

/// The Java `Double.toString` form of a float (design F10): a plain decimal with at least one
/// fraction digit for 1e-3 <= |x| < 1e7, else `d.dddE±n`; `NaN`, `Infinity`, `-Infinity`.
/// The digits are the shortest that read back as `value` (as Java since JDK 19).
fn float_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }
    if (1e-3..1e7).contains(&value.abs()) {
        let mut text = value.to_string();
        if !text.contains('.') {
            text.push_str(".0");
        }
        return text;
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let point = if mantissa.contains('.') { "" } else { ".0" };
    format!("{mantissa}{point}E{exponent}")
}

// ----- errors -----

fn wrong_kind<E>(conversion: Conversion, what: &str, arg: &Arg<E>) -> Failure {
    Failure::Failed(EvalError::invalid_type(
        format!(
            "{} takes {what}, not {}",
            conversion.name(),
            arg.value.kind_name()
        ),
        arg.span,
    ))
}

fn out_of_range<E>(conversion: Conversion, arg: &Arg<E>) -> Failure {
    let target = match conversion.target {
        CastTarget::Integer => "an INTEGER (64-bit)",
        CastTarget::Float => "a FLOAT (64-bit)",
        CastTarget::String | CastTarget::Boolean => "the type",
    };
    let shown = match &arg.value {
        Value::Scalar(ElementProperty::String(text)) => quoted(text),
        Value::Scalar(ElementProperty::Number(number)) => match number.n {
            N::NegInt(value) => value.to_string(),
            N::Float(value) => float_text(value.0),
        },
        other => other.kind_name().to_owned(),
    };
    Failure::Failed(EvalError::new(
        status::OUT_OF_RANGE,
        format!(
            "{}: {shown} is out of range for {target}",
            conversion.name()
        ),
        arg.span,
    ))
}

/// The 22018 error of `CAST` on a string that is no text of the target.
fn invalid_cast<E>(conversion: Conversion, arg: &Arg<E>) -> EvalError {
    let (target, accepted) = match conversion.target {
        CastTarget::Integer => ("an INTEGER", "a decimal number"),
        CastTarget::Float => (
            "a FLOAT",
            "a decimal number, 'NaN', 'Infinity' or '-Infinity'",
        ),
        CastTarget::Boolean => ("a BOOLEAN", "'true', 'false' or 'unknown'"),
        CastTarget::String => ("a STRING", "any text"),
    };
    let text = arg.value.as_str().unwrap_or_default();
    EvalError::new(
        status::INVALID_CAST,
        format!(
            "invalid character value for cast: {} is not {target} (expected {accepted}, \
             surrounding spaces allowed)",
            quoted(text)
        ),
        arg.span,
    )
}

/// The 22G03 error of an element or a path converted to a string, in every mode (design F10).
fn element_to_string<E>(conversion: Conversion, arg: &Arg<E>) -> Failure {
    Failure::Fatal(EvalError::invalid_type(
        format!(
            "{} takes no vertex, edge or path (the argument is {}, or holds one): an element \
             has no text form; use element_id(x) for its id, properties(x) for its \
             properties, or return the element or path itself (RETURN p)",
            conversion.name(),
            arg.value.kind_name()
        ),
        arg.span,
    ))
}

/// `text` in single quotes, cut after [`QUOTED_CHARS`] characters.
fn quoted(text: &str) -> String {
    match text.char_indices().nth(QUOTED_CHARS) {
        Some((at, _)) => format!("'{}...'", &text[..at]),
        None => format!("'{text}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::eval::status::{
        INVALID_CAST, INVALID_VALUE_TYPE, LIMIT, OUT_OF_RANGE,
    };
    use super::super::test_env::*;
    use super::*;

    /// An expected value, or `Err(status)`.
    type Expected = Result<Value<El>, &'static str>;

    /// Checks `(source, [CAST, toX, toXOrNull])` rows.
    fn check(target: &str, function: &str, rows: &[(&str, [Expected; 3])]) {
        for (source, [cast, lenient, or_null]) in rows {
            let calls: [(&str, Expected); 3] = [
                ("CAST", cast.clone()),
                ("", lenient.clone()),
                ("OrNull", or_null.clone()),
            ];
            for (form, expected) in calls {
                let text = match form {
                    "CAST" => format!("CAST({source} AS {target})"),
                    _ => format!("{function}{form}({source})"),
                };
                match (eval(&text), expected) {
                    (Ok(got), Ok(want)) => assert_eq!(got, want, "{text}"),
                    (Err(got), Err(want)) => assert_eq!(got.gqlstatus, want, "{text}: {got:?}"),
                    (got, want) => panic!("{text}: got {got:?}, expected {want:?}"),
                }
            }
        }
    }

    const CAST_ERR: Expected = Err(INVALID_CAST);
    const KIND: Expected = Err(INVALID_VALUE_TYPE);
    const RANGE: Expected = Err(OUT_OF_RANGE);

    fn i(value: i64) -> Expected {
        Ok(int(value))
    }

    fn f(value: f64) -> Expected {
        Ok(float(value))
    }

    fn b(value: bool) -> Expected {
        Ok(boolean(value))
    }

    fn s(text: &str) -> Expected {
        Ok(string(text))
    }

    fn nul() -> Expected {
        Ok(null())
    }

    #[test]
    fn to_integer_by_source_kind_and_mode() {
        check(
            "INTEGER",
            "toInteger",
            &[
                ("NULL", [nul(), nul(), nul()]),
                ("x", [nul(), nul(), nul()]),
                ("42", [i(42), i(42), i(42)]),
                ("$big", [i(i64::MAX), i(i64::MAX), i(i64::MAX)]),
                ("$min", [i(i64::MIN), i(i64::MIN), i(i64::MIN)]),
                ("1.9", [i(1), i(1), i(1)]),
                ("-1.9", [i(-1), i(-1), i(-1)]),
                ("-0.5", [i(0), i(0), i(0)]),
                ("1.0e19", [RANGE, RANGE, nul()]),
                ("-9.3e18", [RANGE, RANGE, nul()]),
                ("TRUE", [KIND, i(1), i(1)]),
                ("FALSE", [KIND, i(0), i(0)]),
                ("'42'", [i(42), i(42), i(42)]),
                ("'-42'", [i(-42), i(-42), i(-42)]),
                ("'+42'", [i(42), i(42), i(42)]),
                ("' 42 '", [i(42), i(42), i(42)]),
                // A tab is no space for CAST, but <= U+0020 for toInteger (Java trim).
                ("'\t42\n'", [CAST_ERR, i(42), i(42)]),
                // U+3000 is whitespace but above U+0020: never trimmed.
                ("'\u{3000}42'", [CAST_ERR, nul(), nul()]),
                ("'1.9'", [i(1), i(1), i(1)]),
                ("'-1.9'", [i(-1), i(-1), i(-1)]),
                ("'.5'", [i(0), i(0), i(0)]),
                ("'5.'", [i(5), i(5), i(5)]),
                ("'1e3'", [i(1000), i(1000), i(1000)]),
                ("'1E+3'", [i(1000), i(1000), i(1000)]),
                ("'12.5e-1'", [i(1), i(1), i(1)]),
                ("'1e-999999999999999999999'", [i(0), i(0), i(0)]),
                ("'0e999999999'", [i(0), i(0), i(0)]),
                ("'0000000000000000000000000042'", [i(42), i(42), i(42)]),
                (
                    "'9223372036854775807'",
                    [i(i64::MAX), i(i64::MAX), i(i64::MAX)],
                ),
                (
                    "'-9223372036854775808'",
                    [i(i64::MIN), i(i64::MIN), i(i64::MIN)],
                ),
                (
                    "'9223372036854775807.99'",
                    [i(i64::MAX), i(i64::MAX), i(i64::MAX)],
                ),
                ("'9223372036854775808'", [RANGE, RANGE, nul()]),
                ("'-9223372036854775809'", [RANGE, RANGE, nul()]),
                ("'1e19'", [RANGE, RANGE, nul()]),
                ("'1e999999999999999999999'", [RANGE, RANGE, nul()]),
                // Cypher literals: toInteger only (P0).
                ("'0x1F'", [CAST_ERR, i(31), i(31)]),
                ("'-0x1f'", [CAST_ERR, i(-31), i(-31)]),
                ("'0o17'", [CAST_ERR, i(15), i(15)]),
                ("'1_000'", [CAST_ERR, i(1000), i(1000)]),
                ("'1_000.9'", [CAST_ERR, i(1000), i(1000)]),
                ("'0x7FFFFFFFFFFFFFFF'", [CAST_ERR, i(i64::MAX), i(i64::MAX)]),
                (
                    "'-0x8000000000000000'",
                    [CAST_ERR, i(i64::MIN), i(i64::MIN)],
                ),
                ("'0x8000000000000000'", [CAST_ERR, RANGE, nul()]),
                (
                    "'0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF'",
                    [CAST_ERR, RANGE, nul()],
                ),
                // Not a number in any reading.
                ("'abc'", [CAST_ERR, nul(), nul()]),
                ("''", [CAST_ERR, nul(), nul()]),
                ("'   '", [CAST_ERR, nul(), nul()]),
                ("'-'", [CAST_ERR, nul(), nul()]),
                ("'.'", [CAST_ERR, nul(), nul()]),
                ("'1e'", [CAST_ERR, nul(), nul()]),
                ("'12abc'", [CAST_ERR, nul(), nul()]),
                ("'NaN'", [CAST_ERR, nul(), nul()]),
                ("'Infinity'", [CAST_ERR, nul(), nul()]),
                ("'0x'", [CAST_ERR, nul(), nul()]),
                ("'0xG'", [CAST_ERR, nul(), nul()]),
                ("'0X1F'", [CAST_ERR, nul(), nul()]),
                ("'_1'", [CAST_ERR, nul(), nul()]),
                ("'1_'", [CAST_ERR, nul(), nul()]),
                ("'1._5'", [CAST_ERR, nul(), nul()]),
                ("'١٢'", [CAST_ERR, nul(), nul()]),
                ("'héllo'", [CAST_ERR, nul(), nul()]),
                ("'日本語 🎉'", [CAST_ERR, nul(), nul()]),
                // Wrong kinds.
                ("[1]", [KIND, KIND, nul()]),
                ("{a: 1}", [KIND, KIND, nul()]),
                ("n", [KIND, KIND, nul()]),
                ("e", [KIND, KIND, nul()]),
                ("p", [KIND, KIND, nul()]),
            ],
        );
    }

    #[test]
    fn to_float_by_source_kind_and_mode() {
        check(
            "FLOAT",
            "toFloat",
            &[
                ("NULL", [nul(), nul(), nul()]),
                ("42", [f(42.0), f(42.0), f(42.0)]),
                (
                    "$big",
                    [f(i64::MAX as f64), f(i64::MAX as f64), f(i64::MAX as f64)],
                ),
                ("2.5", [f(2.5), f(2.5), f(2.5)]),
                ("'2.5'", [f(2.5), f(2.5), f(2.5)]),
                ("' -2.5 '", [f(-2.5), f(-2.5), f(-2.5)]),
                ("'\n2.5'", [CAST_ERR, f(2.5), f(2.5)]),
                ("'1e3'", [f(1000.0), f(1000.0), f(1000.0)]),
                ("'.5'", [f(0.5), f(0.5), f(0.5)]),
                ("'7'", [f(7.0), f(7.0), f(7.0)]),
                ("'1e-400'", [f(0.0), f(0.0), f(0.0)]),
                ("'1e400'", [RANGE, RANGE, nul()]),
                ("'-1e400'", [RANGE, RANGE, nul()]),
                // Non-finite texts (F4, P0), exact case as Java.
                (
                    "'Infinity'",
                    [f(f64::INFINITY), f(f64::INFINITY), f(f64::INFINITY)],
                ),
                (
                    "' -Infinity '",
                    [
                        f(f64::NEG_INFINITY),
                        f(f64::NEG_INFINITY),
                        f(f64::NEG_INFINITY),
                    ],
                ),
                ("'infinity'", [CAST_ERR, nul(), nul()]),
                ("'inf'", [CAST_ERR, nul(), nul()]),
                ("'nan'", [CAST_ERR, nul(), nul()]),
                // Cypher literals: toFloat only.
                ("'0x1F'", [CAST_ERR, f(31.0), f(31.0)]),
                ("'1_000.5'", [CAST_ERR, f(1000.5), f(1000.5)]),
                // Java-only forms are no Cypher literal.
                ("'1.5f'", [CAST_ERR, nul(), nul()]),
                ("'1d'", [CAST_ERR, nul(), nul()]),
                ("'0x1p3'", [CAST_ERR, nul(), nul()]),
                ("'abc'", [CAST_ERR, nul(), nul()]),
                ("''", [CAST_ERR, nul(), nul()]),
                ("TRUE", [KIND, KIND, nul()]),
                ("[1.5]", [KIND, KIND, nul()]),
                ("mp", [KIND, KIND, nul()]),
                ("n", [KIND, KIND, nul()]),
                ("z", [KIND, KIND, nul()]),
            ],
        );
        // NaN is not equal to itself: checked by its kind.
        for text in [
            "CAST('NaN' AS FLOAT)",
            "toFloat(' NaN ')",
            "toFloatOrNull('NaN')",
        ] {
            match value(text) {
                Value::Scalar(ElementProperty::Number(number)) => {
                    assert!(number.as_f64().is_some_and(f64::is_nan), "{text}");
                }
                other => panic!("{text}: {other:?}"),
            }
        }
    }

    #[test]
    fn to_boolean_by_source_kind_and_mode() {
        check(
            "BOOLEAN",
            "toBoolean",
            &[
                ("NULL", [nul(), nul(), nul()]),
                ("TRUE", [b(true), b(true), b(true)]),
                ("FALSE", [b(false), b(false), b(false)]),
                ("'true'", [b(true), b(true), b(true)]),
                ("'FALSE'", [b(false), b(false), b(false)]),
                ("'TrUe'", [b(true), b(true), b(true)]),
                ("' true '", [b(true), b(true), b(true)]),
                // Java whitespace for toBoolean, the space only for CAST.
                ("'\ttrue\u{2003}'", [CAST_ERR, b(true), b(true)]),
                // A non-breaking space is no Java whitespace.
                ("'true\u{a0}'", [CAST_ERR, nul(), nul()]),
                ("'unknown'", [nul(), nul(), nul()]),
                ("'yes'", [CAST_ERR, nul(), nul()]),
                ("'1'", [CAST_ERR, nul(), nul()]),
                ("''", [CAST_ERR, nul(), nul()]),
                ("'tru'", [CAST_ERR, nul(), nul()]),
                ("0", [KIND, b(false), b(false)]),
                ("1", [KIND, b(true), b(true)]),
                ("-7", [KIND, b(true), b(true)]),
                ("$min", [KIND, b(true), b(true)]),
                ("0.0", [KIND, KIND, nul()]),
                ("1.5", [KIND, KIND, nul()]),
                ("[TRUE]", [KIND, KIND, nul()]),
                ("{a: TRUE}", [KIND, KIND, nul()]),
                ("e", [KIND, KIND, nul()]),
                ("p", [KIND, KIND, nul()]),
            ],
        );
        // `'unknown'` is the unknown truth value only for CAST (toBoolean: not a boolean text).
        assert_eq!(value("CAST(' UNKNOWN ' AS BOOLEAN) IS NULL"), boolean(true));
    }

    #[test]
    fn to_string_by_source_kind_and_mode() {
        check(
            "STRING",
            "toString",
            &[
                ("NULL", [nul(), nul(), nul()]),
                ("'héllo'", [s("héllo"), s("héllo"), s("héllo")]),
                (
                    "'日本語 🎉'",
                    [s("日本語 🎉"), s("日本語 🎉"), s("日本語 🎉")],
                ),
                ("''", [s(""), s(""), s("")]),
                ("42", [s("42"), s("42"), s("42")]),
                ("-7", [s("-7"), s("-7"), s("-7")]),
                (
                    "$min",
                    [
                        s("-9223372036854775808"),
                        s("-9223372036854775808"),
                        s("-9223372036854775808"),
                    ],
                ),
                ("1.0", [s("1.0"), s("1.0"), s("1.0")]),
                ("11.5", [s("11.5"), s("11.5"), s("11.5")]),
                ("TRUE", [s("true"), s("true"), s("true")]),
                ("FALSE", [s("false"), s("false"), s("false")]),
                ("[]", [s("[]"), KIND, nul()]),
                ("{}", [s("{}"), KIND, nul()]),
                (
                    "[1, 'a', TRUE, NULL, 2.0]",
                    [s("[1, a, true, null, 2.0]"), KIND, nul()],
                ),
                ("{a: 1, b: 'hi'}", [s("{a: 1, b: hi}"), KIND, nul()]),
                (
                    "{l: [1, [2]], m: {k: NULL}}",
                    [s("{l: [1, [2]], m: {k: null}}"), KIND, nul()],
                ),
                ("$list", [s("[1, 2]"), KIND, nul()]),
                ("n.tags", [s("[a, b]"), KIND, nul()]),
                ("n.address", [s("{city: Berlin}"), KIND, nul()]),
                // Elements and paths: 22G03 (design F10), also nested; `toStringOrNull` gives
                // NULL for an element, a path, a list and a map (stage 3f, U11).
                ("n", [KIND, KIND, nul()]),
                ("e", [KIND, KIND, nul()]),
                ("p", [KIND, KIND, nul()]),
                ("z", [KIND, KIND, nul()]),
                ("[1, n]", [KIND, KIND, nul()]),
                ("{a: [e]}", [KIND, KIND, nul()]),
            ],
        );
    }

    #[test]
    fn float_text_is_the_java_form() {
        for (input, text) in [
            (1.0, "1.0"),
            (-1.0, "-1.0"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (11.5, "11.5"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.3333333333333333"),
            (0.001, "0.001"),
            (0.000_999_9, "9.999E-4"),
            (1.0e-4, "1.0E-4"),
            (1.2345e-5, "1.2345E-5"),
            (9_999_999.0, "9999999.0"),
            (1.0e7, "1.0E7"),
            (-1.0e7, "-1.0E7"),
            (1.5e20, "1.5E20"),
            (123_456_789.0, "1.23456789E8"),
            (f64::MAX, "1.7976931348623157E308"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(float_text(input), text, "{input:e}");
        }
        // Through the functions: the stored float's text, CAST and toString alike.
        check_values(&[
            ("toString(1.5e20)", string("1.5E20")),
            ("CAST(0.0001 AS STRING)", string("1.0E-4")),
            ("toString(e.weight)", string("0.5")),
            ("toString(toFloat('NaN'))", string("NaN")),
            ("toString(-toFloat('Infinity'))", string("-Infinity")),
            ("toString(1.0 / 3)", string("0.3333333333333333")),
        ]);
    }

    #[test]
    fn conversions_round_trip() {
        check_values(&[
            ("toInteger(toString($big))", int(i64::MAX)),
            ("toInteger(toString($min))", int(i64::MIN)),
            ("CAST(CAST(1.5e20 AS STRING) AS FLOAT)", float(1.5e20)),
            ("toFloat(toString(1.0 / 3)) = 1.0 / 3", boolean(true)),
            ("toBoolean(toString(TRUE))", boolean(true)),
            ("CAST(CAST(-42 AS STRING) AS INTEGER)", int(-42)),
            ("toInteger(n.age) + 1", int(30)),
            ("toString(n.age) || '!'", string("29!")),
            ("toIntegerOrNull(n.name)", null()),
            ("toIntegerOrNull(n.stored_null)", null()),
            ("toString($s)", string("ma")),
            ("toInteger($nothing)", null()),
        ]);
    }

    #[test]
    fn errors_name_the_function_and_point_at_the_argument() {
        let error = eval("CAST('abc' AS INTEGER)").unwrap_err();
        assert_eq!(error.gqlstatus, INVALID_CAST);
        assert!(
            error.message.contains("'abc' is not an INTEGER"),
            "{error:?}"
        );
        assert_eq!(
            &"MATCH (n) RETURN CAST('abc' AS INTEGER)"[error.span.start..error.span.end],
            "'abc'"
        );

        let error = eval("toFloat(TRUE)").unwrap_err();
        assert_eq!(error.gqlstatus, INVALID_VALUE_TYPE);
        assert_eq!(
            error.message,
            "toFloat() takes a number or a string, not a boolean"
        );
        assert_eq!(
            &"MATCH (n) RETURN toFloat(TRUE)"[error.span.start..error.span.end],
            "TRUE"
        );

        let error = eval("toInteger('1e30')").unwrap_err();
        assert_eq!(error.gqlstatus, OUT_OF_RANGE);
        assert!(
            error
                .message
                .starts_with("toInteger(): '1e30' is out of range"),
            "{error:?}"
        );

        let error = eval("CAST(1.0e30 AS INTEGER)").unwrap_err();
        assert!(
            error.message.contains("1.0E30 is out of range"),
            "{error:?}"
        );

        let error = eval("toString(n)").unwrap_err();
        assert_eq!(error.gqlstatus, INVALID_VALUE_TYPE);
        for advice in ["element_id(x)", "properties(x)", "RETURN p"] {
            assert!(error.message.contains(advice), "{error:?}");
        }

        // A long text is cut in the message, by characters.
        let long = format!("toIntegerOrNull('{}') IS NULL", "é".repeat(100));
        assert_eq!(value(&long), boolean(true));
        let error = eval(&format!("CAST('{}' AS BOOLEAN)", "é".repeat(100))).unwrap_err();
        assert!(
            error.message.contains(&format!("'{}...'", "é".repeat(40))),
            "{error:?}"
        );
    }

    #[test]
    fn list_and_map_texts_reserve_their_size() {
        // "[1, 2, 3]" is 9 bytes.
        assert_eq!(
            eval_in(&TestRow::with_limit(9), "CAST(l AS STRING)"),
            Ok(string("[1, 2, 3]"))
        );
        for text in ["CAST(l AS STRING)", "CAST([l] AS STRING)"] {
            let error = eval_in(&TestRow::with_limit(8), text).unwrap_err();
            assert_eq!(error.gqlstatus, LIMIT, "{text}");
        }
        // "{a: 1, b: hi}" is 13 bytes.
        assert_eq!(
            eval_in(&TestRow::with_limit(13), "CAST(mp AS STRING)"),
            Ok(string("{a: 1, b: hi}"))
        );
        assert_eq!(
            eval_in(&TestRow::with_limit(12), "CAST(mp AS STRING)")
                .unwrap_err()
                .gqlstatus,
            LIMIT
        );
    }

    #[test]
    fn decimal_truncation_is_exact() {
        for (text, expected) in [
            ("0", Some(0)),
            ("-0", Some(0)),
            ("00.000", Some(0)),
            ("123.456e2", Some(12345)),
            ("123.456e-2", Some(1)),
            ("-123.456e-3", Some(0)),
            ("0.000000000000000000000000000001e30", Some(1)),
            ("922337203685477580.7e1", Some(i64::MAX)),
            ("-922337203685477580.8e1", Some(i64::MIN)),
            ("922337203685477580.8e1", None),
            ("1e9223372036854775807", None),
            ("1e-9223372036854775808", Some(0)),
        ] {
            let decimal = Decimal::parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(decimal.truncate(), expected, "{text}");
        }
        for text in [
            "", "+", "-", ".", "e3", "1e", "1e+", "1.2.3", "1e3e4", " 1", "1 ", "--1",
        ] {
            assert!(Decimal::parse(text).is_none(), "{text:?}");
        }
    }
}
