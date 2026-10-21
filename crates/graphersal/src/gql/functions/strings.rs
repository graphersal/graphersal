//! String functions (stage 1c): `left`, `right`, `substring`, `trim` (strings and the SQL
//! `TRIM` forms), `btrim` / `ltrim` / `rtrim`, `replace`, `split`, `reverse`, `byte_length` /
//! `octet_length`, `isEmpty`, `nullif`.
//!
//! The contract of a family file is in the module docs of `functions/mod.rs`; unit tests use
//! the test `Env` and helpers of `functions/test_env.rs`.
//!
//! Rules (user documentation: the book, `gql/semantics.md`, "Strings: trim, split, replace"):
//!
//! - Every position, length and count is in characters (Unicode scalar values, F1). Results are
//!   built from `chars()` iterators (never by slicing a string at a computed byte offset), and
//!   splitting slices only through `str::get` at the edges of a match `find` returned.
//! - `NULL` in any argument gives `NULL` (F2), except `nullif(a, b)`, which is `a` unless
//!   `a = b` is TRUE.
//! - `left` / `right` / `substring` with a negative count or position: 22011. A one-character
//!   rule broken by `TRIM(c FROM s)`: 22027. A negative `replace` limit: 22023.
//! - The default trim character is the space U+0020 for every trim form (F11, P0); `btrim` /
//!   `ltrim` / `rtrim` strip any character of `chars` (a set; `''` strips nothing).
//! - `replace` (Java `String.replace`, an empty search inserts the replacement before every
//!   character and at the end) and `split` (empty components kept, the leftmost delimiter wins,
//!   on a tie the one listed first; an empty delimiter splits into characters) count their result
//!   first and reserve it (F6) before building it.

use super::super::ast::TrimSide;
use super::super::eval::{Env, EvalError, Truth, Value, equals, status};
use super::super::lexer::Span;
use super::super::parser::Function;
use super::{
    Arg, any_null, function_not_implemented, integer_arg, list_bytes, reserve, string_arg,
    wrong_kind,
};
use crate::CompactString;

/// The default trim character (design F11).
const SPACE: char = ' ';

/// Evaluates the string function `function` (its family, [`super::Family::Strings`]; `trim`
/// with a list first argument goes to `lists.rs`) on its evaluated arguments.
pub(crate) fn call<En: Env>(
    function: Function,
    args: Vec<Arg<En::Element>>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    if function == Function::NullIf {
        return Ok(nullif(args));
    }
    if any_null(&args) {
        return Ok(Value::Null);
    }
    match function {
        Function::Left => left_or_right(function, &args, true),
        Function::Right => left_or_right(function, &args, false),
        Function::Substring => substring(&args),
        Function::Trim => trim_call(&args, span),
        Function::Btrim => set_trim(function, &args, TrimSide::Both),
        Function::Ltrim => set_trim(function, &args, TrimSide::Leading),
        Function::Rtrim => set_trim(function, &args, TrimSide::Trailing),
        Function::Replace => replace(&args, env, span),
        Function::Split => split(&args, env, span),
        Function::Reverse => reverse(args),
        Function::ByteLength => match args.first() {
            Some(arg) => {
                let text = string_arg(function, arg, "a string")?;
                Ok(Value::integer(
                    i64::try_from(text.len()).unwrap_or(i64::MAX),
                ))
            }
            None => Ok(Value::Null),
        },
        Function::IsEmpty => is_empty(&args),
        Function::NullIf => Ok(Value::Null),
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
        | Function::PercentileCont
        | Function::PercentileDisc
        | Function::Cast
        | Function::ToInteger
        | Function::ToIntegerOrNull
        | Function::ToFloat
        | Function::ToFloatOrNull
        | Function::ToBoolean
        | Function::ToBooleanOrNull
        | Function::ToString
        | Function::ToStringOrNull
        | Function::Abs
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
        | Function::Mod
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

/// The SQL forms of `TRIM`: `TRIM([LEADING | TRAILING | BOTH] [character] FROM source)`
/// (`character`: `None` for the default, the space, design F11).
pub(crate) fn trim<En: Env>(
    side: TrimSide,
    character: Option<Arg<En::Element>>,
    source: Arg<En::Element>,
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let _ = (env, span);
    if source.is_null() || character.as_ref().is_some_and(Arg::is_null) {
        return Ok(Value::Null);
    }
    let text = string_arg(Function::Trim, &source, "a string")?;
    let removed = match &character {
        None => SPACE,
        Some(arg) => {
            let given = string_arg(Function::Trim, arg, "a string of one character")?;
            let mut chars = given.chars();
            match (chars.next(), chars.next()) {
                (Some(only), None) => only,
                _ => {
                    return Err(EvalError::new(
                        status::TRIM,
                        format!(
                            "TRIM(.. FROM ..) removes exactly one character, not {} \
                             (use btrim(s, chars) to strip any character of a set)",
                            describe_length(given)
                        ),
                        arg.span,
                    ));
                }
            }
        }
    };
    Ok(Value::string(strip(text, side, |c| c == removed)))
}

/// "an empty string" or "N characters", for the 22027 message.
fn describe_length(text: &str) -> String {
    match text.chars().count() {
        0 => "an empty string".to_owned(),
        count => format!("{count} characters"),
    }
}

/// `text` without the characters `remove` accepts at the ends `side` names.
fn strip(text: &str, side: TrimSide, remove: impl Fn(char) -> bool) -> &str {
    match side {
        TrimSide::Leading => text.trim_start_matches(remove),
        TrimSide::Trailing => text.trim_end_matches(remove),
        TrimSide::Both => text.trim_matches(remove),
    }
}

/// `nullif(a, b)`: `NULL` when `a = b` is TRUE, else `a`.
fn nullif<E: PartialEq>(args: Vec<Arg<E>>) -> Value<E> {
    let mut args = args.into_iter();
    let (Some(a), Some(b)) = (args.next(), args.next()) else {
        return Value::Null;
    };
    match equals(&a.value, &b.value) {
        Truth::True => Value::Null,
        Truth::False | Truth::Unknown => a.value,
    }
}

/// A non-negative character count or position as `usize`, else 22011 at the argument.
fn count_arg<E>(function: Function, arg: &Arg<E>, what: &str) -> Result<usize, EvalError> {
    let value = integer_arg(function, arg, &format!("an integer {what}"))?;
    if value < 0 {
        return Err(EvalError::new(
            status::SUBSTRING,
            format!(
                "{}() takes a {what} of 0 or more, not {value}",
                super::super::eval::function_name(function)
            ),
            arg.span,
        ));
    }
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

/// `left(s, n)` / `right(s, n)`.
fn left_or_right<E>(
    function: Function,
    args: &[Arg<E>],
    from_start: bool,
) -> Result<Value<E>, EvalError> {
    let [source, length] = args else {
        return Ok(Value::Null);
    };
    let text = string_arg(function, source, "a string")?;
    let count = count_arg(function, length, "length")?;
    let result: CompactString = if from_start {
        text.chars().take(count).collect()
    } else {
        let skip = text.chars().count().saturating_sub(count);
        text.chars().skip(skip).collect()
    };
    Ok(Value::string(result))
}

/// `substring(s, start [, length])`: 0-based character positions.
fn substring<E>(args: &[Arg<E>]) -> Result<Value<E>, EvalError> {
    let function = Function::Substring;
    let (source, start, length) = match args {
        [source, start] => (source, start, None),
        [source, start, length] => (source, start, Some(length)),
        _ => return Ok(Value::Null),
    };
    let text = string_arg(function, source, "a string")?;
    let start = count_arg(function, start, "start")?;
    let length = match length {
        Some(length) => count_arg(function, length, "length")?,
        None => usize::MAX,
    };
    let result: CompactString = text.chars().skip(start).take(length).collect();
    Ok(Value::string(result))
}

/// The function form `trim(s)`; `trim(s, x)` with a string `s` is 22G03 naming `btrim` (F13).
fn trim_call<E>(args: &[Arg<E>], span: Span) -> Result<Value<E>, EvalError> {
    let function = Function::Trim;
    match args {
        [source] => {
            let text = string_arg(function, source, "a string (or a list and a count)")?;
            Ok(Value::string(strip(text, TrimSide::Both, |c| c == SPACE)))
        }
        [source, _] => match source.value.as_str() {
            Some(_) => Err(EvalError::invalid_type(
                "trim() of a string takes one argument: use btrim(s, chars) to strip the \
                 characters of a set, or TRIM(c FROM s) for one character",
                span,
            )),
            None => Err(wrong_kind(function, "a string or a list", source)),
        },
        _ => Ok(Value::Null),
    }
}

/// `btrim` / `ltrim` / `rtrim` (`s [, chars]`): strip any character of `chars` (the space by
/// default).
fn set_trim<E>(function: Function, args: &[Arg<E>], side: TrimSide) -> Result<Value<E>, EvalError> {
    let Some(source) = args.first() else {
        return Ok(Value::Null);
    };
    let text = string_arg(function, source, "a string")?;
    let result = match args.get(1) {
        None => strip(text, side, |c| c == SPACE),
        Some(chars) => {
            let set = string_arg(function, chars, "a string of characters to strip")?;
            strip(text, side, |c| set.contains(c))
        }
    };
    Ok(Value::string(result))
}

/// `replace(s, search, replacement [, limit])`.
fn replace<En: Env>(
    args: &[Arg<En::Element>],
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let function = Function::Replace;
    let (source, search, replacement, limit) = match args {
        [source, search, replacement] => (source, search, replacement, None),
        [source, search, replacement, limit] => (source, search, replacement, Some(limit)),
        _ => return Ok(Value::Null),
    };
    let text = string_arg(function, source, "a string")?;
    let search = string_arg(function, search, "a string to search for")?;
    let replacement = string_arg(function, replacement, "a replacement string")?;
    let limit = match limit {
        None => usize::MAX,
        Some(limit) => {
            let value = integer_arg(function, limit, "an integer limit")?;
            if value < 0 {
                return Err(EvalError::new(
                    status::INVALID_PARAMETER,
                    format!("replace() takes a limit of 0 or more, not {value}"),
                    limit.span,
                ));
            }
            usize::try_from(value).unwrap_or(usize::MAX)
        }
    };
    // Count the replacements first (F6): an empty search matches before every character and
    // at the end (Java `String.replace`).
    let count = if search.is_empty() {
        text.chars().count().saturating_add(1).min(limit)
    } else {
        text.matches(search).take(limit).count()
    };
    if count == 0 {
        return Ok(Value::string(text));
    }
    let count = count as u64;
    let bytes = (text.len() as u64)
        .saturating_sub(count.saturating_mul(search.len() as u64))
        .saturating_add(count.saturating_mul(replacement.len() as u64));
    reserve(env, bytes, span)?;
    let result = if search.is_empty() {
        let mut out = String::with_capacity(usize::try_from(bytes).unwrap_or(0));
        let mut left = count;
        for c in text.chars() {
            if left > 0 {
                out.push_str(replacement);
                left -= 1;
            }
            out.push(c);
        }
        if left > 0 {
            out.push_str(replacement);
        }
        out
    } else {
        text.replacen(search, replacement, limit)
    };
    Ok(Value::string(result))
}

/// Calls `piece` with each component of `text` cut at `delimiters` (see the module docs).
fn for_each_piece<'t>(text: &'t str, delimiters: &[&str], mut piece: impl FnMut(&'t str)) {
    if text.is_empty() {
        piece("");
        return;
    }
    if delimiters.iter().any(|d| d.is_empty()) {
        let mut start = 0;
        for (index, _) in text.char_indices().skip(1) {
            piece(text.get(start..index).unwrap_or_default());
            start = index;
        }
        piece(text.get(start..).unwrap_or_default());
        return;
    }
    // The next match of each delimiter at or after `position` (`None`: no more).
    let mut next: Vec<Option<usize>> = vec![Some(0); delimiters.len()];
    let mut stale = vec![true; delimiters.len()];
    let mut position = 0;
    loop {
        let mut best: Option<(usize, usize)> = None;
        for (k, delimiter) in delimiters.iter().enumerate() {
            if stale[k] || next[k].is_some_and(|at| at < position) {
                next[k] = text
                    .get(position..)
                    .and_then(|rest| rest.find(delimiter))
                    .map(|at| position + at);
                stale[k] = false;
            }
            if let Some(at) = next[k]
                && best.is_none_or(|(best_at, _)| at < best_at)
            {
                best = Some((at, delimiter.len()));
            }
        }
        match best {
            Some((at, length)) => {
                piece(text.get(position..at).unwrap_or_default());
                position = at + length;
            }
            None => {
                piece(text.get(position..).unwrap_or_default());
                return;
            }
        }
    }
}

/// `split(s, delimiter)`: `delimiter` a string or a list of strings.
fn split<En: Env>(
    args: &[Arg<En::Element>],
    env: &En,
    span: Span,
) -> Result<Value<En::Element>, EvalError> {
    let function = Function::Split;
    let [source, delimiter] = args else {
        return Ok(Value::Null);
    };
    let text = string_arg(function, source, "a string")?;
    let delimiters: Vec<&str> = match &delimiter.value {
        Value::List(items) => {
            let mut delimiters = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Null => return Ok(Value::Null),
                    other => match other.as_str() {
                        Some(text) => delimiters.push(text),
                        None => {
                            return Err(EvalError::invalid_type(
                                format!(
                                    "split() takes a string or a list of strings as the \
                                     delimiter, not a list containing {}",
                                    other.kind_name()
                                ),
                                delimiter.span,
                            ));
                        }
                    },
                }
            }
            delimiters
        }
        _ => vec![string_arg(
            function,
            delimiter,
            "a string or a list of strings as the delimiter",
        )?],
    };
    let (mut count, mut text_bytes) = (0u64, 0u64);
    for_each_piece(text, &delimiters, |piece| {
        count += 1;
        text_bytes += piece.len() as u64;
    });
    reserve(env, list_bytes(count).saturating_add(text_bytes), span)?;
    let mut pieces = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for_each_piece(text, &delimiters, |piece| pieces.push(Value::string(piece)));
    Ok(Value::List(pieces))
}

/// `reverse(x)`: a string by characters or a list by items.
fn reverse<E>(args: Vec<Arg<E>>) -> Result<Value<E>, EvalError> {
    let Some(arg) = args.into_iter().next() else {
        return Ok(Value::Null);
    };
    match arg.value {
        Value::List(mut items) => {
            items.reverse();
            Ok(Value::List(items))
        }
        ref value => match value.as_str() {
            Some(text) => Ok(Value::string(text.chars().rev().collect::<CompactString>())),
            None => Err(wrong_kind(Function::Reverse, "a string or a list", &arg)),
        },
    }
}

/// `isEmpty(x)`: a string, list or map without characters / items / entries.
fn is_empty<E>(args: &[Arg<E>]) -> Result<Value<E>, EvalError> {
    let Some(arg) = args.first() else {
        return Ok(Value::Null);
    };
    match &arg.value {
        Value::List(items) => Ok(Value::boolean(items.is_empty())),
        Value::Map(entries) => Ok(Value::boolean(entries.is_empty())),
        value => match value.as_str() {
            Some(text) => Ok(Value::boolean(text.is_empty())),
            None => Err(wrong_kind(
                Function::IsEmpty,
                "a string, a list or a map",
                arg,
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::eval::status;
    use super::super::test_env::*;

    fn strings(items: &[&str]) -> Value {
        list(items.iter().map(|item| string(item)).collect())
    }

    type Value = super::super::super::eval::Value<El>;

    #[test]
    fn left_and_right_count_characters() {
        check_values(&[
            ("left('héllo', 2)", string("hé")),
            ("right('héllo', 3)", string("llo")),
            ("left('日本語', 2)", string("日本")),
            ("right('日本語', 1)", string("語")),
            ("left('a🎉b', 2)", string("a🎉")),
            ("right('a🎉b', 2)", string("🎉b")),
            ("left('abc', 0)", string("")),
            ("right('abc', 0)", string("")),
            ("left('abc', 10)", string("abc")),
            ("right('abc', $big)", string("abc")),
            ("left('', 3)", string("")),
            ("left(NULL, 1)", null()),
            ("left('a', NULL)", null()),
            ("right(x, -1)", null()),
        ]);
        check_statuses(&[
            ("left('abc', -1)", status::SUBSTRING),
            ("right('abc', $min)", status::SUBSTRING),
            ("left('abc', 1.0)", status::INVALID_VALUE_TYPE),
            ("left(5, 1)", status::INVALID_VALUE_TYPE),
            ("right(['a'], 1)", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn substring_is_zero_based_by_characters() {
        check_values(&[
            ("substring('héllo', 1)", string("éllo")),
            ("substring('héllo', 1, 3)", string("éll")),
            ("substring('日本語', 1, 1)", string("本")),
            ("substring('x🎉y', 1, 1)", string("🎉")),
            ("substring('abc', 3)", string("")),
            ("substring('abc', 10, 2)", string("")),
            ("substring('abc', 1, 10)", string("bc")),
            ("substring('abc', 0, 0)", string("")),
            ("substring('abc', $big)", string("")),
            ("substring('', 0)", string("")),
            ("substring('abc', NULL)", null()),
            ("substring('abc', 1, NULL)", null()),
        ]);
        check_statuses(&[
            ("substring('abc', -1)", status::SUBSTRING),
            ("substring('abc', 0, -1)", status::SUBSTRING),
            ("substring('abc', 0.5)", status::INVALID_VALUE_TYPE),
            ("substring(1, 0)", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn trim_forms_strip_spaces_by_default() {
        check_values(&[
            ("trim('  a b  ')", string("a b")),
            ("trim('\ta ')", string("\ta")),
            ("trim('')", string("")),
            ("trim('   ')", string("")),
            ("trim(NULL)", null()),
            ("trim(NULL, 2)", null()),
            ("TRIM('  a  ')", string("a")),
            ("TRIM(FROM '  a  ')", string("a")),
            ("TRIM(LEADING FROM '  a  ')", string("a  ")),
            ("TRIM(TRAILING FROM '  a  ')", string("  a")),
            ("TRIM(BOTH 'x' FROM 'xxaxx')", string("a")),
            ("TRIM(LEADING 'x' FROM 'xxaxx')", string("axx")),
            ("TRIM(TRAILING 'x' FROM 'xxaxx')", string("xxa")),
            ("TRIM('é' FROM 'éhéé')", string("h")),
            ("TRIM('🎉' FROM '🎉日本🎉')", string("日本")),
            ("TRIM('x' FROM '')", string("")),
            ("TRIM(NULL FROM 'a')", null()),
            ("TRIM('xy' FROM NULL)", null()),
        ]);
        check_statuses(&[
            ("TRIM('xy' FROM 'xya')", status::TRIM),
            ("TRIM('' FROM 'a')", status::TRIM),
            ("TRIM(1 FROM 'a')", status::INVALID_VALUE_TYPE),
            ("TRIM('x' FROM 1)", status::INVALID_VALUE_TYPE),
            ("trim('a', 'b')", status::INVALID_VALUE_TYPE),
            ("trim(1)", status::INVALID_VALUE_TYPE),
            ("trim(1, 2)", status::INVALID_VALUE_TYPE),
        ]);
        let error = eval("trim('xa', 'x')").unwrap_err();
        assert!(error.message.contains("btrim(s, chars)"), "{error:?}");
        let error = eval("TRIM('xy' FROM 'a')").unwrap_err();
        assert!(error.message.contains("2 characters"), "{error:?}");
    }

    #[test]
    fn btrim_ltrim_rtrim_strip_a_set() {
        check_values(&[
            ("btrim('xxyyhelloxyxy', 'xy')", string("hello")),
            ("ltrim('xxyyhelloxyxy', 'xy')", string("helloxyxy")),
            ("rtrim('xxyyhelloxyxy', 'xy')", string("xxyyhello")),
            ("btrim('  a  ')", string("a")),
            ("ltrim('  a  ')", string("a  ")),
            ("rtrim('  a  ')", string("  a")),
            ("btrim('\ta\t')", string("\ta\t")),
            ("btrim('abc', '')", string("abc")),
            ("btrim('éaé', 'é')", string("a")),
            ("btrim('日本語日', '日語')", string("本")),
            ("btrim('', 'x')", string("")),
            ("btrim('abc', NULL)", null()),
            ("ltrim(NULL)", null()),
        ]);
        check_statuses(&[
            ("btrim(1)", status::INVALID_VALUE_TYPE),
            ("rtrim('a', 1)", status::INVALID_VALUE_TYPE),
            ("ltrim(['a'], 'a')", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn replace_follows_java_and_counts_its_limit() {
        check_values(&[
            ("replace('hello', 'l', 'L')", string("heLLo")),
            ("replace('aaaa', 'aa', 'b')", string("bb")),
            ("replace('aaa', 'aa', 'b')", string("ba")),
            ("replace('abc', '', 'x')", string("xaxbxcx")),
            ("replace('', '', 'x')", string("x")),
            ("replace('', 'a', 'x')", string("")),
            ("replace('héllo', 'é', 'e')", string("hello")),
            ("replace('日本語', '本', '')", string("日語")),
            ("replace('a🎉b🎉', '🎉', '!')", string("a!b!")),
            ("replace('abc', 'z', 'x')", string("abc")),
            ("replace('a.b.c', '.', '--', 1)", string("a--b.c")),
            ("replace('a.b.c', '.', '--', 0)", string("a.b.c")),
            ("replace('a.b.c', '.', '--', $big)", string("a--b--c")),
            ("replace('abc', '', 'x', 2)", string("xaxbc")),
            ("replace('ab', '', 'x', 5)", string("xaxbx")),
            ("replace(NULL, 'a', 'b')", null()),
            ("replace('a', NULL, 'b')", null()),
            ("replace('a', 'a', NULL)", null()),
            ("replace('a', 'a', 'b', NULL)", null()),
        ]);
        check_statuses(&[
            ("replace('a', 'a', 'b', -1)", status::INVALID_PARAMETER),
            ("replace('a', 'a', 'b', 1.0)", status::INVALID_VALUE_TYPE),
            ("replace(1, 'a', 'b')", status::INVALID_VALUE_TYPE),
            ("replace('a', 1, 'b')", status::INVALID_VALUE_TYPE),
            ("replace('a', 'a', 1)", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn replace_reserves_its_result() {
        // 'aaaa' -> 4 x 'bbbb' = 16 bytes.
        let error = eval_in(&TestRow::with_limit(15), "replace('aaaa', 'a', 'bbbb')").unwrap_err();
        assert_eq!(error.gqlstatus, status::LIMIT);
        assert_eq!(
            eval_in(&TestRow::with_limit(16), "replace('aaaa', 'a', 'bbbb')"),
            Ok(string("bbbbbbbbbbbbbbbb"))
        );
        // Nothing replaced: nothing to reserve.
        assert_eq!(
            eval_in(&TestRow::with_limit(0), "replace('aaaa', 'z', 'bbbb')"),
            Ok(string("aaaa"))
        );
    }

    #[test]
    fn split_keeps_empty_components() {
        check_values(&[
            ("split('a,b,c', ',')", strings(&["a", "b", "c"])),
            ("split('a,,b', ',')", strings(&["a", "", "b"])),
            ("split('a,', ',')", strings(&["a", ""])),
            ("split(',', ',')", strings(&["", ""])),
            ("split('', ',')", strings(&[""])),
            ("split('abc', 'abc')", strings(&["", ""])),
            ("split('abc', 'z')", strings(&["abc"])),
            ("split('a--b', '--')", strings(&["a", "b"])),
            ("split('abc', '')", strings(&["a", "b", "c"])),
            ("split('', '')", strings(&[""])),
            ("split('h€l🎉', '')", strings(&["h", "€", "l", "🎉"])),
            ("split('日本,語', ',')", strings(&["日本", "語"])),
            ("split('a,b;c', [',', ';'])", strings(&["a", "b", "c"])),
            ("split('a,;b', [',', ';'])", strings(&["a", "", "b"])),
            // A tie at one position: the delimiter listed first wins.
            ("split('a--b', ['-', '--'])", strings(&["a", "", "b"])),
            ("split('a--b', ['--', '-'])", strings(&["a", "b"])),
            // Leftmost wins over listed first.
            ("split('a;b,c', [',', ';'])", strings(&["a", "b", "c"])),
            ("split('ab', ['x', ''])", strings(&["a", "b"])),
            ("split('ab', [])", strings(&["ab"])),
            ("split('', [])", strings(&[""])),
            ("split('a,b', [',', NULL])", null()),
            ("split(NULL, ',')", null()),
            ("split('a', NULL)", null()),
        ]);
        check_statuses(&[
            ("split('a', 1)", status::INVALID_VALUE_TYPE),
            ("split('a', [',', 1])", status::INVALID_VALUE_TYPE),
            ("split(1, ',')", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn split_reserves_its_result() {
        let error = eval_in(&TestRow::with_limit(10), "split('a,b,c', ',')").unwrap_err();
        assert_eq!(error.gqlstatus, status::LIMIT);
        let error = eval_in(&TestRow::with_limit(10), "split('abc', '')").unwrap_err();
        assert_eq!(error.gqlstatus, status::LIMIT);
    }

    #[test]
    fn reverse_strings_and_lists() {
        check_values(&[
            ("reverse('héllo')", string("olléh")),
            ("reverse('日本語🎉')", string("🎉語本日")),
            ("reverse('')", string("")),
            (
                "reverse([1, 'a', NULL])",
                list(vec![null(), string("a"), int(1)]),
            ),
            ("reverse([])", list(vec![])),
            ("reverse(l)", list(vec![int(3), int(2), int(1)])),
            ("reverse(NULL)", null()),
        ]);
        check_statuses(&[
            ("reverse(1)", status::INVALID_VALUE_TYPE),
            ("reverse(mp)", status::INVALID_VALUE_TYPE),
            ("reverse(n)", status::INVALID_VALUE_TYPE),
            ("reverse(p)", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn byte_length_counts_utf8_bytes() {
        check_values(&[
            ("byte_length('abc')", int(3)),
            ("byte_length('héllo')", int(6)),
            ("octet_length('日本語')", int(9)),
            ("byte_length('🎉')", int(4)),
            ("byte_length('')", int(0)),
            ("byte_length(NULL)", null()),
        ]);
        check_statuses(&[
            ("byte_length(1)", status::INVALID_VALUE_TYPE),
            ("octet_length(['a'])", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn is_empty_of_strings_lists_and_maps() {
        check_values(&[
            ("isEmpty('')", boolean(true)),
            ("isEmpty(' ')", boolean(false)),
            ("isEmpty([])", boolean(true)),
            ("isEmpty(l)", boolean(false)),
            ("isEmpty({})", boolean(true)),
            ("isEmpty(mp)", boolean(false)),
            ("isEmpty(NULL)", null()),
        ]);
        check_statuses(&[
            ("isEmpty(0)", status::INVALID_VALUE_TYPE),
            ("isEmpty(n)", status::INVALID_VALUE_TYPE),
            ("isEmpty(p)", status::INVALID_VALUE_TYPE),
        ]);
    }

    #[test]
    fn nullif_is_null_only_on_equality() {
        check_values(&[
            ("nullif(1, 1)", null()),
            ("nullif(1, 1.0)", null()),
            ("nullif('a', 'a')", null()),
            ("nullif(1, 2)", int(1)),
            ("nullif('a', 1)", string("a")),
            ("nullif(1, NULL)", int(1)),
            ("nullif(NULL, 1)", null()),
            ("nullif(NULL, NULL)", null()),
            ("nullif([1, NULL], [1, NULL])", list(vec![int(1), null()])),
            ("nullif(l, [1, 2, 3])", null()),
            ("nullif(n, n)", null()),
        ]);
        assert_eq!(value("nullif(n, m)"), value("n"));
    }

    #[test]
    fn errors_point_at_the_argument() {
        let error = eval("left('abc', -1)").unwrap_err();
        assert!(error.message.contains("length of 0 or more"), "{error:?}");
        // `compiled` evaluates `MATCH (n) RETURN <expr>`.
        let query = "MATCH (n) RETURN left('abc', -1)";
        assert_eq!(&query[error.span.start..error.span.end], "-1");
        let error = eval("replace('a', 'a', 'b', -2)").unwrap_err();
        let query = "MATCH (n) RETURN replace('a', 'a', 'b', -2)";
        assert_eq!(&query[error.span.start..error.span.end], "-2");
        let error = eval("TRIM('xy' FROM 'a')").unwrap_err();
        let query = "MATCH (n) RETURN TRIM('xy' FROM 'a')";
        assert_eq!(&query[error.span.start..error.span.end], "'xy'");
    }
}
