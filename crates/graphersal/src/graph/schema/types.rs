use std::fmt;

use crate::value::ElementProperty;
use crate::value::VectorElement;
use crate::value::temporal::TemporalKind;

/// The type of an [`ElementProperty`] property value: every leaf corresponds to exactly one
/// `ElementProperty` variant.
///
/// `ValueType` is the shadow of the runtime value model, not a separate type system. It names
/// the type of a property value for [`GType`](crate::value::GType) tokens, casts and
/// diagnostics. The leaves `Null`, `Boolean`, `Int64`, `Float64`, `String`, `Uuid` and the five
/// temporal types each describe one kind of `ElementProperty` value; `Array`, `Object` and `Union` are containers.
/// What a *schema* declares is a [`PropertySchema`](crate::graph::schema::PropertySchema) (a
/// JSON Schema node), whose diagnostics use the same names.
///
/// Type names have one canonical spelling everywhere, and it is this type's [`fmt::Display`]
/// form: `null`, `boolean`, `int64`, `float64`, `string`, `uuid`, `date`, `localdatetime`,
/// `datetime`, `time`, `duration`, `vector<f32>` (and the other element kinds), `array<T>`, `object`, `union<A|B>`. In a schema they are written in JSON Schema's vocabulary (`integer` = `int64`,
/// `number` = `float64`, `string` + `format: "uuid"` = `uuid`).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ValueType {
    /// The absence of a value.
    Null,
    /// A boolean.
    Boolean,
    /// A 64-bit signed integer.
    Int64,
    /// A 64-bit IEEE-754 float.
    Float64,
    /// A UTF-8 string.
    String,
    /// A UUID.
    Uuid,
    /// A calendar day ([`Date`](crate::value::temporal::Date)).
    Date,
    /// A wall-clock date and time without a zone
    /// ([`LocalDateTime`](crate::value::temporal::LocalDateTime)).
    LocalDateTime,
    /// An instant with its UTC offset ([`DateTime`](crate::value::temporal::DateTime)).
    DateTime,
    /// A time of day ([`Time`](crate::value::temporal::Time)).
    Time,
    /// A length of time in months, days and nanoseconds
    /// ([`Duration`](crate::value::temporal::Duration)).
    Duration,
    /// A time of day with a UTC offset ([`OffsetTime`](crate::value::temporal::OffsetTime)).
    OffsetTime,
    /// A vector of one element kind ([`Vector`](crate::value::Vector)), any dimension.
    Vector(VectorElement),
    /// A homogeneous sequence of `T`.
    Array(Box<ValueType>),
    /// A map of named fields.
    Object,
    /// A value that may take any one of several types.
    ///
    /// The empty union `union<>` has no value at all: it is the item type [`type_of`] reports
    /// for an empty array (`array<union<>>`).
    Union(Vec<ValueType>),
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueType::Null => f.write_str("null"),
            ValueType::Boolean => f.write_str("boolean"),
            ValueType::Int64 => f.write_str("int64"),
            ValueType::Float64 => f.write_str("float64"),
            ValueType::String => f.write_str("string"),
            ValueType::Uuid => f.write_str("uuid"),
            ValueType::Date => f.write_str("date"),
            ValueType::LocalDateTime => f.write_str("localdatetime"),
            ValueType::DateTime => f.write_str("datetime"),
            ValueType::Time => f.write_str("time"),
            ValueType::Duration => f.write_str("duration"),
            ValueType::OffsetTime => f.write_str("offsettime"),
            ValueType::Vector(element) => write!(f, "vector<{element}>"),
            ValueType::Array(inner) => write!(f, "array<{inner}>"),
            ValueType::Object => f.write_str("object"),
            ValueType::Union(variants) => {
                f.write_str("union<")?;
                for (index, variant) in variants.iter().enumerate() {
                    if index > 0 {
                        f.write_str("|")?;
                    }
                    write!(f, "{variant}")?;
                }
                f.write_str(">")
            }
        }
    }
}

/// Returns the exact logical [`ValueType`] of a property value, or `None` for a value that is
/// not a property value.
///
/// The mapping follows the `ElementProperty` variant and nothing else: a string stays
/// [`ValueType::String`] even when its content looks like a UUID, a date or an IP address
/// (CLAUDE.md rule 7), while a real [`ElementProperty::Uuid`] is [`ValueType::Uuid`]. Arrays
/// report the unified type of their items. An empty array has no item to take a type from: its
/// item type is the empty union (`array<union<>>`), never an invented `null`.
///
/// A graph element ([`ElementProperty::Vertex`]/[`ElementProperty::Edge`]) is a stream value,
/// never a property value, so it has no `ValueType`: it — and any array or object that contains
/// one at any depth — yields `None`.
///
/// The match is exhaustive on purpose: a new `ElementProperty` variant does not compile until it
/// is mapped here.
pub fn type_of(prop: &ElementProperty) -> Option<ValueType> {
    match prop {
        ElementProperty::Null => Some(ValueType::Null),
        ElementProperty::Boolean(_) => Some(ValueType::Boolean),
        ElementProperty::Number(n) => Some(if n.is_i64() {
            ValueType::Int64
        } else {
            ValueType::Float64
        }),
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => Some(ValueType::String),
        ElementProperty::Uuid(_) => Some(ValueType::Uuid),
        ElementProperty::Temporal(value) => Some(ValueType::of_temporal(value.kind())),
        ElementProperty::Vector(value) => Some(ValueType::Vector(value.element())),
        ElementProperty::Array(arr) => {
            let mut variants = Vec::new();
            for item in arr.iter() {
                flatten_union(&mut variants, type_of(item)?);
            }
            // No item, no item type: the empty union.
            Some(ValueType::Array(Box::new(if variants.len() == 1 {
                variants.swap_remove(0)
            } else {
                ValueType::Union(variants)
            })))
        }
        ElementProperty::Object(map) => {
            for value in map.values() {
                type_of(value)?;
            }
            Some(ValueType::Object)
        }
        // A map with a non-string key is still an `object`.
        ElementProperty::Map(map) => {
            for value in map.values() {
                type_of(value)?;
            }
            Some(ValueType::Object)
        }
        ElementProperty::Vertex(_) | ElementProperty::Edge(_) => None,
    }
}

impl ValueType {
    /// The value type of a temporal kind.
    pub(crate) fn of_temporal(kind: TemporalKind) -> ValueType {
        match kind {
            TemporalKind::Date => ValueType::Date,
            TemporalKind::LocalDateTime => ValueType::LocalDateTime,
            TemporalKind::DateTime => ValueType::DateTime,
            TemporalKind::Time => ValueType::Time,
            TemporalKind::Duration => ValueType::Duration,
            TemporalKind::OffsetTime => ValueType::OffsetTime,
        }
    }

    /// The temporal kind of a temporal value type; `None` for every other type.
    pub(crate) fn temporal_kind(&self) -> Option<TemporalKind> {
        match self {
            ValueType::Date => Some(TemporalKind::Date),
            ValueType::LocalDateTime => Some(TemporalKind::LocalDateTime),
            ValueType::DateTime => Some(TemporalKind::DateTime),
            ValueType::Time => Some(TemporalKind::Time),
            ValueType::Duration => Some(TemporalKind::Duration),
            ValueType::OffsetTime => Some(TemporalKind::OffsetTime),
            ValueType::Null
            | ValueType::Boolean
            | ValueType::Int64
            | ValueType::Float64
            | ValueType::String
            | ValueType::Uuid
            | ValueType::Vector(_)
            | ValueType::Array(_)
            | ValueType::Object
            | ValueType::Union(_) => None,
        }
    }
}

/// Returns the display name of a value's type for diagnostics: its [`type_of`] rendered in the
/// canonical spelling, or `vertex`/`edge` for a graph element.
///
/// A container that holds a graph element somewhere inside is named by its shape
/// (`array`/`object`), since it has no `ValueType`.
pub(crate) fn value_type_name(value: &ElementProperty) -> String {
    // A vector names its dimension too (`vector<f32, 768>`), as a declaration does.
    if let ElementProperty::Vector(vector) = value {
        return format!("vector<{}, {}>", vector.element(), vector.dimension());
    }
    if let Some(ty) = type_of(value) {
        return ty.to_string();
    }
    match value {
        ElementProperty::Vertex(_) => "vertex".to_string(),
        ElementProperty::Edge(_) => "edge".to_string(),
        ElementProperty::Array(_) => "array".to_string(),
        _ => "object".to_string(),
    }
}

/// Appends `ty` to a flat union variant list, de-duplicating (`null` stays a member: an array
/// may hold nulls).
fn flatten_union(out: &mut Vec<ValueType>, ty: ValueType) {
    match ty {
        ValueType::Union(inner) => {
            for item in inner {
                flatten_union(out, item);
            }
        }
        other => {
            if !out.contains(&other) {
                out.push(other);
            }
        }
    }
}

/// The upper bound, in bytes, of one compiled schema `pattern` (the `regex` crate's
/// `size_limit`). A schema can come from an untrusted file; this keeps one pattern from claiming
/// unbounded memory at compile time.
pub(crate) const PATTERN_SIZE_LIMIT: usize = 1 << 20;

/// A compiled `pattern` constraint: a regular expression a string value must match.
///
/// The semantics are JSON Schema's: the pattern is **not** implicitly anchored (a value matches
/// when the expression matches anywhere in it; write `^...$` for a whole-value match), and the
/// syntax is the ECMA-262-like subset the [`regex`](https://docs.rs/regex) crate supports:
/// literals, `.`, character classes (`[a-z]`, `[^0-9]`), `\d`/`\w`/`\s`, anchors `^`/`$`,
/// groups, alternation `|`, and the quantifiers `*`, `+`, `?`, `{n}`, `{n,}`, `{n,m}` (also lazy).
/// Look-around and back-references are not supported (matching stays linear in the input) and
/// make the pattern invalid. Deviation from ECMA-262: `\d`, `\w`, `\s` and `\b` are
/// Unicode-aware (`\d` also matches other decimal digits such as `٣`); write `[0-9]` for ASCII
/// digits.
///
/// The expression is compiled once, when the pattern is built ([`SchemaPattern::new`], or when a
/// schema file is parsed), never per validated value; an invalid expression is a
/// [`SchemaError::InvalidPattern`](crate::error::SchemaError::InvalidPattern). Two patterns are
/// equal when their source texts are.
#[derive(Clone)]
pub struct SchemaPattern {
    regex: regex::Regex,
}

impl SchemaPattern {
    /// Compiles `pattern`, failing with
    /// [`SchemaError::InvalidPattern`](crate::error::SchemaError::InvalidPattern) when it is not a
    /// valid expression (or compiles to more than an internal size limit).
    pub fn new(pattern: impl AsRef<str>) -> Result<Self, crate::error::SchemaError> {
        let pattern = pattern.as_ref();
        regex::RegexBuilder::new(pattern)
            .size_limit(PATTERN_SIZE_LIMIT)
            .build()
            .map(|regex| Self { regex })
            .map_err(|err| crate::error::SchemaError::InvalidPattern {
                pattern: pattern.to_string(),
                reason: pattern_error_reason(&err),
            })
    }

    /// The pattern's source text, as written in the schema.
    pub fn as_str(&self) -> &str {
        self.regex.as_str()
    }

    /// `true` when the expression matches somewhere in `value` (JSON Schema `pattern`).
    pub fn is_match(&self, value: &str) -> bool {
        self.regex.is_match(value)
    }
}

/// The one-line reason of a pattern that does not compile: the `regex` crate's message without
/// its multi-line source excerpt.
pub(crate) fn pattern_error_reason(err: &regex::Error) -> String {
    match err {
        regex::Error::Syntax(text) => text
            .lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix("error: "))
            .unwrap_or(text.as_str())
            .to_string(),
        regex::Error::CompiledTooBig(limit) => {
            format!("the compiled expression exceeds the size limit of {limit} bytes")
        }
        other => other.to_string(),
    }
}

impl PartialEq for SchemaPattern {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl fmt::Debug for SchemaPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SchemaPattern")
            .field(&self.as_str())
            .finish()
    }
}

impl fmt::Display for SchemaPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
