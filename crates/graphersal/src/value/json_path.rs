use std::sync::Arc;

use crate::error::ValueError;
use crate::{CompactString, ElementProperty, GraphStorage, TraverserError, TraverserValue};

/// One step of a parsed [`JsonPath`]: either an object member name or an array index.
///
/// Part of the storage contract: a storage that writes or removes below a property itself (an
/// override of `GraphStorage::set_vertex_property_path` and the like) walks
/// [`JsonPath::segments`]. Bracket names (`['a.b']`) and dot names give the same `Key`; the
/// leading `$` is not a segment.
///
/// ```
/// use graphersal::storage::{JsonPath, JsonPathSegment};
///
/// let path: JsonPath = "$.meta['a.b'][-1]".parse().unwrap();
/// assert_eq!(
///     path.segments(),
///     [
///         JsonPathSegment::Key("meta".into()),
///         JsonPathSegment::Key("a.b".into()),
///         JsonPathSegment::Index(-1),
///     ]
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JsonPathSegment {
    /// An object member name.
    Key(CompactString),
    /// An array index; a negative value counts from the end (`-1` is the last element).
    Index(i64),
}

/// Resolves a possibly negative index against an array length. `None` when out of range.
pub(crate) fn resolve_index(index: i64, len: usize) -> Option<usize> {
    if index >= 0 {
        usize::try_from(index).ok().filter(|i| *i < len)
    } else {
        len.checked_sub(usize::try_from(index.unsigned_abs()).ok()?)
    }
}

/// A parsed, reusable JSONPath expression restricted to RFC 9535 *singular queries*: a chain of
/// object member names and array indices that addresses at most one value, e.g. `$.meta.tags[0]`,
/// `meta.tags[-1].name`, `$['a.b'].c`. The leading `$` is optional. Names are written in dot
/// notation (`.name`) or bracket notation (`['a.b']` / `["a.b"]`, with `\'`, `\"`, `\\` and the
/// RFC 9535 control escapes); indices are integers, negative ones counting from the end.
///
/// Deliberately unsupported (rejected at parse time with a clear message): recursive descent
/// (`..`), wildcards (`*`), filters (`?`), slices (`[1:2]`) and unions (`[0,1]`). They address
/// several values and need subtree materialization, at odds with this engine's lazy steps; the
/// singular subset keeps every lookup as cheap as a `key.key2` chain.
///
/// Dot notation is lenient: a name runs up to the next `.` or `[` and may contain any character
/// except `]`, `*`, `?`, quotes and whitespace (so `user-name` works, as before). `Display`
/// renders the canonical form `$.a.b[1].c`, using the bracket form only for names that are not
/// RFC 9535 member-name shorthands.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JsonPath(pub(crate) Arc<Vec<JsonPathSegment>>);

impl JsonPath {
    /// The segments of the path in order, the first one naming the stored property of a write
    /// (see [`JsonPathSegment`]). Parse a path with `str::parse` (`"a.b[1]".parse::<JsonPath>()`).
    pub fn segments(&self) -> &[JsonPathSegment] {
        &self.0
    }
}

/// True when `name` is an RFC 9535 member-name-shorthand (`name-first *name-char`).
fn is_shorthand_name(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = |c: char| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii();
    match chars.next() {
        Some(c) if first_ok(c) => chars.all(|c| first_ok(c) || c.is_ascii_digit()),
        _ => false,
    }
}

fn write_quoted(f: &mut std::fmt::Formatter<'_>, name: &str) -> std::fmt::Result {
    f.write_str("['")?;
    for c in name.chars() {
        match c {
            '\'' => f.write_str("\\'")?,
            '\\' => f.write_str("\\\\")?,
            '\u{8}' => f.write_str("\\b")?,
            '\u{c}' => f.write_str("\\f")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
            c => write!(f, "{c}")?,
        }
    }
    f.write_str("']")
}

/// Writes the canonical form of `segments`; with `wildcard` every index is written `[*]`.
fn write_segments(
    f: &mut std::fmt::Formatter<'_>,
    segments: &[JsonPathSegment],
    wildcard: bool,
) -> std::fmt::Result {
    write!(f, "$")?;
    for segment in segments {
        match segment {
            JsonPathSegment::Key(key) if is_shorthand_name(key) => write!(f, ".{key}")?,
            JsonPathSegment::Key(key) => write_quoted(f, key)?,
            JsonPathSegment::Index(_) if wildcard => f.write_str("[*]")?,
            JsonPathSegment::Index(index) => write!(f, "[{index}]")?,
        }
    }
    Ok(())
}

impl std::fmt::Display for JsonPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_segments(f, &self.0, false)
    }
}

/// The canonical path text `rendered` (`$.a[1].b`) with every array index written `[*]`
/// (`$.a[*].b`); `None` when it is not a path or has no index. Display only: a `[*]` path
/// does not parse back (wildcards are not part of the singular subset).
pub(crate) fn wildcard_indices(rendered: &str) -> Option<String> {
    struct Wildcard<'a>(&'a [JsonPathSegment]);
    impl std::fmt::Display for Wildcard<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write_segments(f, self.0, true)
        }
    }
    if !rendered.starts_with('$') || !rendered.contains('[') {
        return None;
    }
    let segments = parse_json_path(rendered).ok()?;
    segments
        .iter()
        .any(|s| matches!(s, JsonPathSegment::Index(_)))
        .then(|| Wildcard(&segments).to_string())
}

impl std::str::FromStr for JsonPath {
    type Err = TraverserError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        parse_json_path(raw)
            .map(|segments| JsonPath(Arc::new(segments)))
            .map_err(|reason| {
                TraverserError::Value(ValueError::InvalidJsonPath {
                    path: raw.to_string(),
                    reason,
                })
            })
    }
}

/// Largest index magnitude RFC 9535 allows (2^53 - 1, the interoperable JSON integer range).
const MAX_INDEX: i64 = (1 << 53) - 1;

/// Most segments one path may have. A write creates one nesting level per segment and the
/// write, removal and lookup walks follow the segments one by one, so an unbounded path would
/// build an unboundedly deep value (see [`crate::value::depth`]). Twice the default value-depth
/// limit: a write deeper than `traversal.max_value_depth` is refused by the step anyway.
pub(crate) const MAX_SEGMENTS: usize = 256;

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    len: usize,
}

impl<'a> Lexer<'a> {
    fn new(raw: &'a str) -> Self {
        Lexer {
            chars: raw.char_indices().peekable(),
            len: raw.len(),
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().map(|(_, c)| *c)
    }

    fn pos(&mut self) -> usize {
        self.chars.peek().map_or(self.len, |(i, _)| *i)
    }

    fn bump(&mut self) -> Option<char> {
        self.chars.next().map(|(_, c)| c)
    }

    fn skip_blanks(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.bump();
        }
    }
}

fn parse_json_path(raw: &str) -> Result<Vec<JsonPathSegment>, String> {
    let mut lx = Lexer::new(raw);
    let mut segments = Vec::new();

    if lx.peek() == Some('$') {
        lx.bump();
    }
    // A leading '.' (`.a.b`, `$.a.b`) is accepted; the dot is then handled like any other.
    let mut first = true;
    while let Some(c) = lx.peek() {
        match c {
            '.' => {
                let at = lx.pos();
                lx.bump();
                match lx.peek() {
                    Some('.') => {
                        return Err(format!(
                            "recursive descent ('..') is not supported (at position {at}); use a literal path like \"$.a.b\""
                        ));
                    }
                    Some('[') => {
                        return Err(format!(
                            "unexpected '[' after '.' at position {}; write \"a[0]\" or \"a['b']\", not \"a.[0]\"",
                            lx.pos()
                        ));
                    }
                    None => {
                        return Err(format!(
                            "empty segment: path ends with '.' (at position {at})"
                        ));
                    }
                    Some(_) => segments.push(JsonPathSegment::Key(lex_dot_name(&mut lx)?)),
                }
            }
            '[' => segments.push(lex_bracket(&mut lx)?),
            _ if first => segments.push(JsonPathSegment::Key(lex_dot_name(&mut lx)?)),
            other => {
                return Err(format!(
                    "unexpected '{other}' at position {}; expected '.' or '['",
                    lx.pos()
                ));
            }
        }
        first = false;
        if segments.len() > MAX_SEGMENTS {
            return Err(format!(
                "too many segments: a path has at most {MAX_SEGMENTS} (the segment ending at \
                 position {} is number {})",
                lx.pos(),
                segments.len()
            ));
        }
    }

    if segments.is_empty() {
        return Err("empty JSONPath".to_string());
    }
    Ok(segments)
}

/// Lexes a dot-notation name: everything up to the next `.` or `[`.
fn lex_dot_name(lx: &mut Lexer<'_>) -> Result<CompactString, String> {
    let mut name = String::new();
    let start = lx.pos();
    while let Some(c) = lx.peek() {
        match c {
            '.' | '[' => break,
            '*' => {
                return Err(format!(
                    "wildcards ('*') are not supported (at position {}); name each key explicitly, or quote a key that contains '*' as ['a*b']",
                    lx.pos()
                ));
            }
            '?' => {
                return Err(format!(
                    "filter expressions ('?') are not supported (at position {}); quote a key that contains '?' as ['a?b']",
                    lx.pos()
                ));
            }
            ']' | '\'' | '"' | ' ' | '\t' | '\n' | '\r' => {
                return Err(format!(
                    "unexpected '{c}' in a dot-notation name at position {}; use bracket notation ['...'] for unusual keys",
                    lx.pos()
                ));
            }
            c => {
                name.push(c);
                lx.bump();
            }
        }
    }
    if name.is_empty() {
        return Err(format!(
            "empty segment at position {start}; expected a key name"
        ));
    }
    Ok(CompactString::new(name))
}

/// Lexes one `[...]` selector (the lexer sits on the `[`): a quoted name or an integer index.
fn lex_bracket(lx: &mut Lexer<'_>) -> Result<JsonPathSegment, String> {
    let open = lx.pos();
    lx.bump();
    lx.skip_blanks();
    let segment = match lx.peek() {
        None => return Err(format!("unterminated '[' at position {open}")),
        Some(q @ ('\'' | '"')) => {
            lx.bump();
            JsonPathSegment::Key(CompactString::new(lex_quoted(lx, q, open)?))
        }
        Some('*') => {
            return Err(format!(
                "wildcards ('[*]') are not supported (at position {open}); name each key explicitly"
            ));
        }
        Some('?') => {
            return Err(format!(
                "filter expressions ('[?...]') are not supported (at position {open})"
            ));
        }
        Some(c) if c == '-' || c.is_ascii_digit() => JsonPathSegment::Index(lex_index(lx)?),
        Some(':') => {
            return Err(format!(
                "slices ('[a:b]') are not supported (at position {open})"
            ));
        }
        Some(c) => {
            return Err(format!(
                "unexpected '{c}' in brackets at position {}; expected an integer index or a quoted name",
                lx.pos()
            ));
        }
    };
    lx.skip_blanks();
    match lx.peek() {
        Some(']') => {
            lx.bump();
            Ok(segment)
        }
        Some(':') => Err(format!(
            "slices ('[a:b]') are not supported (at position {open})"
        )),
        Some(',') => Err(format!(
            "unions ('[a,b]') are not supported (at position {open}); a path addresses exactly one value"
        )),
        Some(c) => Err(format!(
            "unexpected '{c}' at position {}; expected ']'",
            lx.pos()
        )),
        None => Err(format!("unterminated '[' at position {open}")),
    }
}

fn lex_index(lx: &mut Lexer<'_>) -> Result<i64, String> {
    let at = lx.pos();
    let mut text = String::new();
    if lx.peek() == Some('-') {
        text.push('-');
        lx.bump();
    }
    while let Some(c) = lx.peek() {
        if c.is_ascii_digit() {
            text.push(c);
            lx.bump();
        } else {
            break;
        }
    }
    let digits = text.trim_start_matches('-');
    if digits.is_empty() {
        return Err(format!(
            "invalid array index '{text}' at position {at}: expected digits after '-'"
        ));
    }
    if text == "-0" {
        return Err(format!(
            "invalid array index '-0' at position {at}: negative zero is not allowed"
        ));
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return Err(format!(
            "invalid array index '{text}' at position {at}: leading zeros are not allowed"
        ));
    }
    match text.parse::<i64>() {
        // `checked_abs`: `i64::MIN` has no positive counterpart (a plain `abs()` overflows).
        Ok(n) if n.checked_abs().is_some_and(|abs| abs <= MAX_INDEX) => Ok(n),
        _ => Err(format!(
            "array index '{text}' at position {at} is out of range (at most +/-{MAX_INDEX})"
        )),
    }
}

/// Lexes the body of a quoted name after the opening quote, through the closing quote.
fn lex_quoted(lx: &mut Lexer<'_>, quote: char, open: usize) -> Result<String, String> {
    let mut out = String::new();
    loop {
        let Some(c) = lx.bump() else {
            return Err(format!(
                "unterminated quoted name starting at position {open}"
            ));
        };
        match c {
            c if c == quote => return Ok(out),
            '\\' => {
                let at = lx.pos();
                let Some(e) = lx.bump() else {
                    return Err(format!(
                        "unterminated quoted name starting at position {open}"
                    ));
                };
                match e {
                    '\'' => out.push('\''),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => out.push(lex_unicode(lx, at)?),
                    other => {
                        return Err(format!(
                            "invalid escape '\\{other}' at position {}",
                            at.saturating_sub(1)
                        ));
                    }
                }
            }
            c if (c as u32) < 0x20 => {
                return Err(format!(
                    "unescaped control character in quoted name at position {}",
                    lx.pos().saturating_sub(1)
                ));
            }
            c => out.push(c),
        }
    }
}

fn lex_hex4(lx: &mut Lexer<'_>, at: usize) -> Result<u32, String> {
    let mut v = 0u32;
    for _ in 0..4 {
        let d = lx.bump().and_then(|c| c.to_digit(16)).ok_or_else(|| {
            format!("invalid '\\u' escape at position {at}: expected 4 hex digits")
        })?;
        v = v * 16 + d;
    }
    Ok(v)
}

fn lex_unicode(lx: &mut Lexer<'_>, at: usize) -> Result<char, String> {
    let hi = lex_hex4(lx, at)?;
    let code = if (0xD800..0xDC00).contains(&hi) {
        // High surrogate: must be followed by `\uDC00..DFFF`.
        if lx.bump() != Some('\\') || lx.bump() != Some('u') {
            return Err(format!("lone surrogate in '\\u' escape at position {at}"));
        }
        let lo = lex_hex4(lx, at)?;
        if !(0xDC00..0xE000).contains(&lo) {
            return Err(format!(
                "invalid surrogate pair in '\\u' escape at position {at}"
            ));
        }
        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
    } else {
        hi
    };
    char::from_u32(code)
        .ok_or_else(|| format!("invalid unicode escape at position {at}: lone surrogate"))
}

/// Descends into an already-materialized, map-or-array-shaped stream value one segment at a
/// time. `None` means "not found" (a missing key or an out-of-range index) - the same silent-empty
/// outcome `values(missingKey)` already gives, not an error; a genuinely malformed path is
/// rejected earlier, at parse time.
pub(crate) fn walk_json_path<S: GraphStorage>(
    mut current: TraverserValue<S>,
    segments: &[JsonPathSegment],
) -> Option<TraverserValue<S>> {
    for segment in segments {
        current = match (segment, current) {
            (JsonPathSegment::Key(key), TraverserValue::Object(obj)) => {
                let lookup = TraverserValue::Computed(ElementProperty::String(key.clone()), None);
                obj.get(&lookup)?.clone()
            }
            (JsonPathSegment::Key(key), TraverserValue::Record { keys, values }) => {
                let idx = keys.iter().position(|k| k == key.as_str())?;
                values[idx].clone()
            }
            (
                JsonPathSegment::Key(key),
                TraverserValue::Computed(ElementProperty::Object(obj), _),
            ) => obj.get(key.as_str())?.clone().into(),
            // Only string keys are addressable by path; a non-string key is unreachable.
            (JsonPathSegment::Key(key), TraverserValue::Computed(ElementProperty::Map(map), _)) => {
                map.get_str(key.as_str())?.clone().into()
            }
            (JsonPathSegment::Index(index), TraverserValue::Array(arr)) => {
                arr.get(resolve_index(*index, arr.len())?)?.clone()
            }
            (
                JsonPathSegment::Index(index),
                TraverserValue::Computed(ElementProperty::Array(arr), _),
            ) => arr.get(resolve_index(*index, arr.len())?)?.clone().into(),
            _ => return None,
        };
    }
    Some(current)
}

/// Descends into a stored property value by reference, one segment at a time, without cloning
/// anything. `None` means "not found": a missing key, an out-of-range index, or a type mismatch
/// (key on an array/scalar, index on an object/scalar). Only string keys are addressable in a
/// non-string-keyed `Map`. Negative indices count from the end ([`resolve_index`]).
pub(crate) fn descend_property<'a>(
    root: &'a ElementProperty,
    segments: &[JsonPathSegment],
) -> Option<&'a ElementProperty> {
    let mut current = root;
    for segment in segments {
        current = match (segment, current) {
            (JsonPathSegment::Key(key), ElementProperty::Object(obj)) => obj.get(key.as_str())?,
            (JsonPathSegment::Key(key), ElementProperty::Map(map)) => map.get_str(key.as_str())?,
            (JsonPathSegment::Index(index), ElementProperty::Array(arr)) => {
                arr.get(resolve_index(*index, arr.len())?)?
            }
            _ => return None,
        };
    }
    Some(current)
}

/// Like [`descend_property`], starting from a whole property map (a vertex's or edge's stored
/// properties): the first segment must be a `Key` naming the stored property. The caller clones
/// only the returned leaf, never the top-level property.
pub(crate) fn descend_properties<'a>(
    props: &'a crate::value::PropertyMap,
    segments: &[JsonPathSegment],
) -> Option<&'a ElementProperty> {
    let (JsonPathSegment::Key(first), rest) = segments.split_first()? else {
        return None;
    };
    descend_property(props.get(first.as_str())?, rest)
}

// ---------------------------------------------------------------------------------------------
// Write side (`property(jpath(..), value)`)
// ---------------------------------------------------------------------------------------------

use crate::error::PathWriteFault;
use crate::value::PropertyMap;

fn value_kind(value: &ElementProperty) -> &'static str {
    match value {
        ElementProperty::Null => "null",
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => "string",
        ElementProperty::Number(_) => "number",
        ElementProperty::Boolean(_) => "boolean",
        ElementProperty::Uuid(_) => "uuid",
        ElementProperty::Temporal(value) => value.kind().name(),
        ElementProperty::Vector(_) => "vector",
        ElementProperty::Blob(_) => "blob",
        ElementProperty::Object(_) => "object",
        ElementProperty::Array(_) => "array",
        ElementProperty::Vertex(_) => "vertex",
        ElementProperty::Edge(_) => "edge",
        ElementProperty::Map(_) => "map",
    }
}

/// Canonical text of the first `depth` segments (error messages only).
fn prefix_text(segments: &[JsonPathSegment], depth: usize) -> String {
    JsonPath(Arc::new(segments[..depth].to_vec())).to_string()
}

/// The fault for a write that needs an array which does not exist: names the array an index
/// segment at or after `from` would address. `None` when no index segment follows.
fn missing_array(segments: &[JsonPathSegment], from: usize) -> Option<PathWriteFault> {
    let position = segments[from..]
        .iter()
        .position(|s| matches!(s, JsonPathSegment::Index(_)))?;
    Some(PathWriteFault::MissingArray {
        at: prefix_text(segments, from + position),
    })
}

/// The value a missing chain of key segments creates: nested objects ending in `value`.
fn build_chain(keys: &[JsonPathSegment], value: ElementProperty) -> ElementProperty {
    let mut built = value;
    for segment in keys.iter().rev() {
        if let JsonPathSegment::Key(key) = segment {
            let mut object = PropertyMap::default();
            object.insert(key.clone(), built);
            built = ElementProperty::Object(Box::new(object));
        }
    }
    built
}

/// Writes `value` at `segments[depth..]` below `current`, in place.
///
/// Missing object keys are created as objects, but only when every remaining segment is a key
/// (an index never creates an array). `[n]` inside an array replaces, `[len]` appends (last
/// segment only), a negative index counts from the end. Every fault is found before anything is
/// mutated: the mutation happens at the bottom of the descent only.
fn write_below(
    current: &mut ElementProperty,
    segments: &[JsonPathSegment],
    depth: usize,
    value: ElementProperty,
) -> Result<(), PathWriteFault> {
    let Some(segment) = segments.get(depth) else {
        *current = value;
        return Ok(());
    };
    let is_last = depth + 1 == segments.len();
    match (segment, current) {
        (JsonPathSegment::Key(key), ElementProperty::Object(object)) => {
            if let Some(child) = object.get_mut(key.as_str()) {
                return write_below(child, segments, depth + 1, value);
            }
            let rest = &segments[depth + 1..];
            if let Some(fault) = missing_array(segments, depth + 1) {
                return Err(fault);
            }
            object.insert(key.clone(), build_chain(rest, value));
            Ok(())
        }
        (JsonPathSegment::Key(_), other) => Err(PathWriteFault::KeyOnNonObject {
            at: prefix_text(segments, depth),
            found: value_kind(other),
        }),
        (JsonPathSegment::Index(index), ElementProperty::Array(array)) => {
            let len = array.len();
            if let Some(position) = resolve_index(*index, len) {
                return write_below(&mut array[position], segments, depth + 1, value);
            }
            if is_last && usize::try_from(*index).is_ok_and(|i| i == len) {
                array.push(value);
                return Ok(());
            }
            Err(PathWriteFault::IndexOutOfRange {
                at: prefix_text(segments, depth),
                index: *index,
                len,
            })
        }
        (JsonPathSegment::Index(_), other) => Err(PathWriteFault::IndexOnNonArray {
            at: prefix_text(segments, depth),
            found: value_kind(other),
        }),
    }
}

/// The property name of a write path: its first segment, which must be a key.
pub(crate) fn write_property_name(path: &JsonPath) -> Result<&CompactString, PathWriteFault> {
    match path.segments().first() {
        Some(JsonPathSegment::Key(name)) => Ok(name),
        _ => Err(PathWriteFault::FirstSegmentNotName),
    }
}

/// Writes `value` at `path` into a property map, in place: the first segment is the stored
/// property (created when missing), the rest descends into its value. See [`write_below`].
pub(crate) fn write_path_in_element(
    properties: &mut PropertyMap,
    path: &JsonPath,
    value: ElementProperty,
) -> Result<(), PathWriteFault> {
    let name = write_property_name(path)?;
    let segments = path.segments();
    match properties.get_mut(name.as_str()) {
        Some(top) => write_below(top, segments, 1, value),
        None => {
            let rest = &segments[1..];
            if let Some(fault) = missing_array(segments, 1) {
                return Err(fault);
            }
            properties.insert(name.clone(), build_chain(rest, value));
            Ok(())
        }
    }
}

/// The whole top-level property that results from writing `value` at `path`, computed on a copy
/// of `existing` (used when the result must go through the schema layer like a plain write).
pub(crate) fn write_path_to_copy(
    existing: Option<&ElementProperty>,
    path: &JsonPath,
    value: ElementProperty,
) -> Result<(CompactString, ElementProperty), PathWriteFault> {
    let name = write_property_name(path)?;
    let segments = path.segments();
    let resulting = match existing {
        Some(top) => {
            let mut copy = top.clone();
            write_below(&mut copy, segments, 1, value)?;
            copy
        }
        None => {
            let rest = &segments[1..];
            if let Some(fault) = missing_array(segments, 1) {
                return Err(fault);
            }
            build_chain(rest, value)
        }
    };
    Ok((name.clone(), resulting))
}

/// Removes the target of `segments[depth..]` below `current`, in place; `true` when something
/// was removed. A missing key, an out-of-range index or a type mismatch on the way (key step on
/// an array or scalar, index step on an object or scalar, a non-string-keyed `Map`) means "the
/// path does not exist": nothing changes and it is not an error. An object key is removed with
/// its position closing up (`shift_remove`), an array element with shift (`Vec::remove`); a
/// parent that becomes empty stays.
fn remove_below(current: &mut ElementProperty, segments: &[JsonPathSegment], depth: usize) -> bool {
    let Some(segment) = segments.get(depth) else {
        return false;
    };
    let is_last = depth + 1 == segments.len();
    match (segment, current) {
        (JsonPathSegment::Key(key), ElementProperty::Object(object)) => {
            if is_last {
                object.shift_remove(key.as_str()).is_some()
            } else {
                object
                    .get_mut(key.as_str())
                    .is_some_and(|child| remove_below(child, segments, depth + 1))
            }
        }
        (JsonPathSegment::Index(index), ElementProperty::Array(array)) => {
            let Some(position) = resolve_index(*index, array.len()) else {
                return false;
            };
            if is_last {
                array.remove(position);
                true
            } else {
                remove_below(&mut array[position], segments, depth + 1)
            }
        }
        _ => false,
    }
}

/// Removes the value at `path` (two or more segments) from a property map, in place; `true`
/// when something was removed. The first segment names the stored property. See [`remove_below`].
pub(crate) fn remove_path_in_element(properties: &mut PropertyMap, path: &JsonPath) -> bool {
    let segments = path.segments();
    let Some(JsonPathSegment::Key(name)) = segments.first() else {
        return false;
    };
    match properties.get_mut(name.as_str()) {
        Some(top) => remove_below(top, segments, 1),
        None => false,
    }
}

/// The whole top-level property that results from removing `path`, computed on a copy of
/// `existing`; `None` when the path does not exist (nothing to store). Used when the result must
/// go through the schema layer like a plain write. The copy is made only when the path exists.
pub(crate) fn remove_path_to_copy(
    existing: Option<&ElementProperty>,
    path: &JsonPath,
) -> Option<(CompactString, ElementProperty)> {
    let segments = path.segments();
    let Some(JsonPathSegment::Key(name)) = segments.first() else {
        return None;
    };
    let top = existing?;
    descend_property(top, &segments[1..])?;
    let mut copy = top.clone();
    if remove_below(&mut copy, segments, 1) {
        Some((name.clone(), copy))
    } else {
        // A path that reads (a string-keyed `Map` entry) but cannot be removed.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dollar_dot_path() {
        let path: JsonPath = "$.meta.k".parse().unwrap();
        assert_eq!(
            path.segments(),
            &[
                JsonPathSegment::Key(CompactString::new("meta")),
                JsonPathSegment::Key(CompactString::new("k")),
            ]
        );
    }

    #[test]
    fn parses_bare_dot_path_without_dollar() {
        let path: JsonPath = "meta.k".parse().unwrap();
        assert_eq!(
            path.segments(),
            &[
                JsonPathSegment::Key(CompactString::new("meta")),
                JsonPathSegment::Key(CompactString::new("k")),
            ]
        );
    }

    #[test]
    fn parses_array_index() {
        let path: JsonPath = "$.tags[0]".parse().unwrap();
        assert_eq!(
            path.segments(),
            &[
                JsonPathSegment::Key(CompactString::new("tags")),
                JsonPathSegment::Index(0),
            ]
        );
    }

    #[test]
    fn parses_chained_index() {
        let path: JsonPath = "$.matrix[0][1]".parse().unwrap();
        assert_eq!(
            path.segments(),
            &[
                JsonPathSegment::Key(CompactString::new("matrix")),
                JsonPathSegment::Index(0),
                JsonPathSegment::Index(1),
            ]
        );
    }

    #[test]
    fn rejects_recursive_descent() {
        let err = "$..k".parse::<JsonPath>().unwrap_err();
        assert!(matches!(
            err,
            TraverserError::Value(ValueError::InvalidJsonPath { .. })
        ));
    }

    #[test]
    fn rejects_wildcard() {
        assert!("$.*".parse::<JsonPath>().is_err());
    }

    #[test]
    fn rejects_bad_index() {
        assert!("$.tags[x]".parse::<JsonPath>().is_err());
    }

    #[test]
    fn rejects_empty_path() {
        assert!("".parse::<JsonPath>().is_err());
        assert!("$".parse::<JsonPath>().is_err());
    }

    fn keys(path: &str) -> Vec<JsonPathSegment> {
        path.parse::<JsonPath>().unwrap().segments().to_vec()
    }

    fn k(s: &str) -> JsonPathSegment {
        JsonPathSegment::Key(CompactString::new(s))
    }

    fn err_reason(path: &str) -> String {
        match path.parse::<JsonPath>().unwrap_err() {
            TraverserError::Value(ValueError::InvalidJsonPath { path: p, reason }) => {
                assert_eq!(p, path);
                reason
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parses_bracket_names() {
        assert_eq!(keys("$['a.b'].c"), vec![k("a.b"), k("c")]);
        assert_eq!(
            keys("$[\"a.b\"][0]"),
            vec![k("a.b"), JsonPathSegment::Index(0)]
        );
        assert_eq!(keys("['a'].b"), vec![k("a"), k("b")]);
        assert_eq!(keys("a['b c']"), vec![k("a"), k("b c")]);
        assert_eq!(keys("$[ 'a' ]"), vec![k("a")]);
    }

    #[test]
    fn parses_escapes() {
        assert_eq!(keys(r"$['it\'s']"), vec![k("it's")]);
        assert_eq!(keys(r#"$["say \"hi\""]"#), vec![k("say \"hi\"")]);
        assert_eq!(keys(r"$['a\\b']"), vec![k("a\\b")]);
        assert_eq!(keys(r"$['a\nb\t']"), vec![k("a\nb\t")]);
        assert_eq!(keys(r"$['\u0041\ud83d\ude00']"), vec![k("A\u{1F600}")]);
    }

    #[test]
    fn quoted_keys_may_contain_reserved_characters() {
        assert_eq!(keys("$['a*b']"), vec![k("a*b")]);
        assert_eq!(keys("$['a?b']"), vec![k("a?b")]);
        assert_eq!(keys("$['a..b']"), vec![k("a..b")]);
        assert_eq!(keys("$['a[1]']"), vec![k("a[1]")]);
        assert_eq!(keys("$['*']"), vec![k("*")]);
    }

    #[test]
    fn parses_negative_and_leading_index() {
        assert_eq!(keys("a[-1]"), vec![k("a"), JsonPathSegment::Index(-1)]);
        assert_eq!(keys("$[2].a"), vec![JsonPathSegment::Index(2), k("a")]);
        assert_eq!(keys("a[ 3 ]"), vec![k("a"), JsonPathSegment::Index(3)]);
        assert_eq!(keys(".a.b"), vec![k("a"), k("b")]);
        assert_eq!(keys("user-name.x"), vec![k("user-name"), k("x")]);
    }

    #[test]
    fn rejects_unsupported_syntax_with_clear_messages() {
        assert!(err_reason("$..k").contains("recursive descent"));
        assert!(err_reason("a..b").contains("recursive descent"));
        assert!(err_reason("$.*").contains("wildcard"));
        assert!(err_reason("$[*]").contains("wildcard"));
        assert!(err_reason("a[?(@.x)]").contains("filter"));
        assert!(err_reason("a.b?").contains("filter"));
        assert!(err_reason("a[1:2]").contains("slice"));
        assert!(err_reason("a[:2]").contains("slice"));
        assert!(err_reason("a[0,1]").contains("union"));
        assert!(err_reason("a['x','y']").contains("union"));
    }

    #[test]
    fn rejects_malformed_syntax_with_clear_messages() {
        assert!(err_reason("a.").contains("empty segment"));
        assert!(err_reason("$.").contains("empty segment"));
        assert!(err_reason("a.[0]").contains("unexpected '['"));
        assert!(err_reason("a['b").contains("unterminated quoted"));
        assert!(err_reason("a[\"b]").contains("unterminated quoted"));
        assert!(err_reason("a[0").contains("unterminated '['"));
        assert!(err_reason("a[").contains("unterminated '['"));
        assert!(err_reason("a[x]").contains("expected an integer index"));
        assert!(err_reason("a[1x]").contains("expected ']'"));
        assert!(err_reason("a[]").contains("expected an integer index"));
        assert!(err_reason("a]").contains("unexpected ']'"));
        assert!(err_reason("a['b']c").contains("expected '.' or '['"));
        assert!(err_reason(r"a['\q']").contains("invalid escape"));
        assert!(err_reason(r"a['\u12']").contains("hex digits"));
        assert!(err_reason(r"a['\ud83d']").contains("surrogate"));
        assert!(err_reason("a['b\nc']").contains("control character"));
        assert!(err_reason("a b").contains("unexpected ' '"));
    }

    #[test]
    fn rejects_non_canonical_integers() {
        assert!(err_reason("a[01]").contains("leading zeros"));
        assert!(err_reason("a[-01]").contains("leading zeros"));
        assert!(err_reason("a[-0]").contains("negative zero"));
        assert!(err_reason("a[-]").contains("expected digits"));
        assert!(err_reason("a[9007199254740992]").contains("out of range"));
        assert!(err_reason("a[99999999999999999999]").contains("out of range"));
        assert_eq!(keys("a[0]"), vec![k("a"), JsonPathSegment::Index(0)]);
        assert_eq!(
            keys("a[9007199254740991]"),
            vec![k("a"), JsonPathSegment::Index(9007199254740991)]
        );
    }

    #[test]
    fn display_is_canonical() {
        let d = |s: &str| s.parse::<JsonPath>().unwrap().to_string();
        assert_eq!(d("a.b[1].c"), "$.a.b[1].c");
        assert_eq!(d("$.a.b[1].c"), "$.a.b[1].c");
        assert_eq!(d("$['a'].b[-1]"), "$.a.b[-1]");
        assert_eq!(d("$['a.b'].c"), "$['a.b'].c");
        assert_eq!(d("$[\"it's\"]"), "$['it\\'s']");
        assert_eq!(d("user-name"), "$['user-name']");
        assert_eq!(d("$['1a']"), "$['1a']");
        assert_eq!(d("$['_x9']"), "$._x9");
        assert_eq!(d("$['']"), "$['']");
    }

    #[test]
    fn display_round_trips() {
        let cases = [
            "a",
            "a.b[1].c",
            "$['a.b'][0][-2]",
            "$['it\\'s']",
            "['x y'].z",
            "$['']",
            r"$['a\\b\nc\u0001']",
            "[0][1]",
            "$['*']['?']",
            "a[0].b['c.d']",
        ];
        for c in cases {
            let p: JsonPath = c.parse().unwrap();
            let again: JsonPath = p.to_string().parse().unwrap();
            assert_eq!(p, again, "round trip of {c} via {p}");
        }
    }

    #[test]
    fn resolve_index_handles_negative_and_range() {
        assert_eq!(resolve_index(0, 3), Some(0));
        assert_eq!(resolve_index(2, 3), Some(2));
        assert_eq!(resolve_index(3, 3), None);
        assert_eq!(resolve_index(-1, 3), Some(2));
        assert_eq!(resolve_index(-3, 3), Some(0));
        assert_eq!(resolve_index(-4, 3), None);
        assert_eq!(resolve_index(-1, 0), None);
        assert_eq!(resolve_index(0, 0), None);
    }

    mod descent {
        use super::*;
        use crate::value::PropertyMap;

        fn obj(entries: Vec<(&str, ElementProperty)>) -> ElementProperty {
            let mut e = PropertyMap::default();
            for (k, v) in entries {
                e.insert(CompactString::new(k), v);
            }
            ElementProperty::Object(Box::new(e))
        }
        fn int(i: i64) -> ElementProperty {
            ElementProperty::Number(i.into())
        }
        fn sample() -> ElementProperty {
            obj(vec![
                (
                    "a",
                    obj(vec![(
                        "b",
                        ElementProperty::Array(vec![int(10), int(20), obj(vec![("c", int(30))])]),
                    )]),
                ),
                ("s", ElementProperty::String("x".into())),
            ])
        }
        fn at<'a>(root: &'a ElementProperty, p: &str) -> Option<&'a ElementProperty> {
            descend_property(root, p.parse::<JsonPath>().unwrap().segments())
        }

        #[test]
        fn descends_objects_and_arrays_by_reference() {
            let root = sample();
            assert_eq!(at(&root, "a.b[0]"), Some(&int(10)));
            assert_eq!(at(&root, "a.b[2].c"), Some(&int(30)));
            assert!(std::ptr::eq(
                at(&root, "s").unwrap(),
                root.as_object().unwrap().get("s").unwrap()
            ));
        }

        #[test]
        fn negative_index_counts_from_end() {
            let root = sample();
            assert_eq!(at(&root, "a.b[-1].c"), Some(&int(30)));
            assert_eq!(at(&root, "a.b[-3]"), Some(&int(10)));
            assert_eq!(at(&root, "a.b[-4]"), None);
        }

        #[test]
        fn missing_and_out_of_range_are_none() {
            let root = sample();
            assert_eq!(at(&root, "zz"), None);
            assert_eq!(at(&root, "a.zz"), None);
            assert_eq!(at(&root, "a.b[3]"), None);
        }

        #[test]
        fn type_mismatch_is_none() {
            let root = sample();
            assert_eq!(at(&root, "a.b.c"), None); // key on array
            assert_eq!(at(&root, "a[0]"), None); // index on object
            assert_eq!(at(&root, "s.x"), None); // key on scalar
            assert_eq!(at(&root, "s[0]"), None); // index on scalar
        }

        #[test]
        fn map_with_non_string_keys_is_reachable_by_string_key() {
            let map = ElementProperty::from_entries(vec![
                (int(1), int(2)),
                (ElementProperty::String("k".into()), int(3)),
            ]);
            assert_eq!(at(&map, "k"), Some(&int(3)));
            assert_eq!(at(&map, "q"), None);
        }

        #[test]
        fn descend_properties_uses_first_segment_as_stored_key() {
            let props = sample().as_object().unwrap().clone();
            let segs = |p: &str| p.parse::<JsonPath>().unwrap().segments().to_vec();
            assert_eq!(descend_properties(&props, &segs("a.b[1]")), Some(&int(20)));
            assert_eq!(descend_properties(&props, &segs("s")), props.get("s"));
            assert_eq!(descend_properties(&props, &segs("nope.b")), None);
            assert_eq!(
                descend_properties(&props, &[JsonPathSegment::Index(0)]),
                None
            );
            assert_eq!(descend_properties(&props, &[]), None);
        }
    }
}
