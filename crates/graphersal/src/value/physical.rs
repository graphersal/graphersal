//! The physical codec: the only place where logical values cross the I/O boundary.
//!
//! Graphersal has two value levels:
//!
//! - **logical**, [`ElementProperty`], the only runtime model (it has a real `Uuid`);
//! - **physical**, [`PhysicalValue`], JSON types only (`null`, `bool`, `int`, `float`, `string`,
//!   `array`, `object`), which exists only at I/O boundaries.
//!
//! [`lower`] turns a logical value into a physical one and needs no schema, because the value
//! knows its own type (a `Uuid` becomes its canonical string). [`lift`] turns a physical value
//! back into a logical one: without a declared type every physical type maps to the logical type
//! of the same name (a string stays a `String`, whatever it looks like: CLAUDE.md rule 7); with a
//! declared [`ValueType`] the value goes through the schema's write coercion and must conform.
//!
//! [`write_json`] and [`read_json`] are a small RFC 8259 text writer and reader for formats that
//! embed JSON inside another format (GraphML writes an `array`/`object` value as JSON text). They
//! need no dependency, so this whole module compiles without any feature; only the serde glue at
//! the bottom is behind the `serde` feature.

use std::borrow::Cow;
use std::fmt;

use compact_str::CompactString;

use crate::graph::schema::PropertySchema;
use crate::graph::schema::coerce::{coerce_to_declared, conforms_to};
use crate::graph::schema::types::{ValueType, value_type_name};
use crate::value::uuid::format_canonical;
use crate::value::{EdgeValue, ElementProperty, N, Number, PropertyMap, VertexValue};

/// A value made of JSON types only.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PhysicalValue<'a> {
    /// JSON `null`.
    Null,
    /// JSON `true`/`false`.
    Bool(bool),
    /// A JSON integer that fits `i64`.
    Int(i64),
    /// A JSON number with a fractional part or an exponent. May be non-finite until written.
    Float(f64),
    /// A JSON string.
    String(Cow<'a, str>),
    /// A JSON array.
    Array(Vec<PhysicalValue<'a>>),
    /// A JSON object; insertion order is preserved.
    Object(Vec<(Cow<'a, str>, PhysicalValue<'a>)>),
}

/// Why a physical value could not be lifted to its declared logical type.
#[cfg_attr(not(feature = "io"), allow(dead_code))] // Consumed by GraphML import (`io`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LiftError {
    /// The declared type's write coercion rejected the value.
    Coercion {
        /// The declared type, in canonical spelling.
        declared: String,
        /// Why the coercion failed.
        reason: &'static str,
    },
    /// The (possibly coerced) value does not conform to the declared type.
    TypeMismatch {
        /// The declared type, in canonical spelling.
        declared: String,
        /// The value's actual type, in canonical spelling.
        actual: String,
    },
}

impl fmt::Display for LiftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiftError::Coercion { declared, reason } => {
                write!(
                    f,
                    "cannot coerce value to declared type {declared}: {reason}"
                )
            }
            LiftError::TypeMismatch { declared, actual } => {
                write!(
                    f,
                    "value of type {actual} does not conform to declared type {declared}"
                )
            }
        }
    }
}

/// Why a value could not be represented physically.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PhysicalError {
    /// JSON has no representation for `NaN` or an infinity.
    NonFiniteFloat(f64),
    /// An unsigned integer above `i64::MAX`, which the logical model cannot hold exactly.
    #[cfg_attr(not(feature = "serde"), allow(dead_code))]
    // Only serde reads unsigned integers.
    IntegerOutOfRange(u64),
    /// Writing to the output sink failed.
    Write,
}

impl fmt::Display for PhysicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PhysicalError::NonFiniteFloat(value) => {
                write!(
                    f,
                    "non-finite float ({value}) cannot be represented in JSON"
                )
            }
            PhysicalError::IntegerOutOfRange(value) => {
                write!(f, "u64 ({value}) exceeding i64::MAX")
            }
            PhysicalError::Write => f.write_str("failed to write JSON output"),
        }
    }
}

impl From<fmt::Error> for PhysicalError {
    fn from(_: fmt::Error) -> Self {
        PhysicalError::Write
    }
}

/// How a leaf [`ValueType`] is encoded at the I/O boundary.
///
/// Internal on purpose: the engine may change the encoding of any type freely, without that
/// being a breaking change for a schema or a query. Containers have no encoding: every format
/// expands them structurally.
#[cfg_attr(not(feature = "io"), allow(dead_code))] // Consumed by GraphML import (`io`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    /// Encoded as an absent value.
    Null,
    /// Encoded as a boolean.
    Boolean,
    /// Encoded as a 64-bit signed integer.
    Int64,
    /// Encoded as a 64-bit float.
    Float64,
    /// Encoded as a UTF-8 string.
    String,
}

/// Returns the physical encoding of a leaf type, or `None` for a container.
#[cfg_attr(not(feature = "io"), allow(dead_code))] // Consumed by GraphML import (`io`).
pub(crate) fn encoding_of(ty: &ValueType) -> Option<Encoding> {
    match ty {
        ValueType::Null => Some(Encoding::Null),
        ValueType::Boolean => Some(Encoding::Boolean),
        ValueType::Int64 => Some(Encoding::Int64),
        ValueType::Float64 => Some(Encoding::Float64),
        // A UUID and a temporal value are carried as their canonical text.
        ValueType::String
        | ValueType::Uuid
        | ValueType::Date
        | ValueType::LocalDateTime
        | ValueType::DateTime
        | ValueType::Time
        | ValueType::OffsetTime
        | ValueType::Duration => Some(Encoding::String),
        // A vector is carried as the list of its values (a container in every format).
        ValueType::Vector(_) | ValueType::Array(_) | ValueType::Object | ValueType::Union(_) => {
            None
        }
    }
}

/// Lowers a logical value to its physical form, borrowing wherever it can.
///
/// `Vertex`/`Edge` values (query results, never stored properties) lower to the untyped
/// GraphSON 3 shape: a vertex `{"id", "label", "properties"}` (plus `"labels"` for a vertex with
/// several labels, a Graphersal extension), an edge `{"id", "label", "inVLabel", "outVLabel",
/// "inV", "outV", "properties"}`; a property named `id` or `label` stays inside `"properties"`.
pub(crate) fn lower(value: &ElementProperty) -> PhysicalValue<'_> {
    match value {
        ElementProperty::Null => PhysicalValue::Null,
        ElementProperty::Boolean(b) => PhysicalValue::Bool(*b),
        ElementProperty::Number(number) => lower_number(number),
        ElementProperty::String(s) => PhysicalValue::String(Cow::Borrowed(s.as_str())),
        // The codec boundary (D5): files and exports hold the plain string.
        ElementProperty::Compressed(text) => {
            PhysicalValue::String(Cow::Owned(text.text().into_string()))
        }
        ElementProperty::Deduped(text) => PhysicalValue::String(Cow::Borrowed(text.as_str())),
        ElementProperty::Uuid(u) => {
            PhysicalValue::String(Cow::Owned(format_canonical(*u).into_string()))
        }
        // The canonical ISO 8601 / RFC 3339 text; [`lift_temporal`] reads it back.
        ElementProperty::Temporal(t) => PhysicalValue::String(Cow::Owned(t.to_string())),
        // The list of its values, dequantized (an i8 or bit vector as its f32 values): a JSON
        // reader gets numbers, and a declared vector property turns them back into the vector
        // (`quantize` of the dequantized values gives the same codes).
        ElementProperty::Vector(v) => crate::value::vector::tagged::lower_values(v),
        ElementProperty::Array(items) => PhysicalValue::Array(items.iter().map(lower).collect()),
        ElementProperty::Object(map) => lower_map(map),
        ElementProperty::Vertex(vertex) => lower_vertex(vertex),
        ElementProperty::Edge(edge) => lower_edge(edge),
        // GraphSON 3 shape: `{"@type": "g:Map", "@value": [k1, v1, k2, v2, ...]}`. Output only: `lift` does
        // not interpret the tag (a value-keyed map is never storable, so it never needs to be read back).
        ElementProperty::Map(map) => {
            let mut flat = Vec::with_capacity(map.len() * 2);
            for (key, value) in map.iter() {
                flat.push(lower(key));
                flat.push(lower(value));
            }
            PhysicalValue::Object(vec![
                (
                    Cow::Borrowed("@type"),
                    PhysicalValue::String(Cow::Borrowed("g:Map")),
                ),
                (Cow::Borrowed("@value"), PhysicalValue::Array(flat)),
            ])
        }
    }
}

fn lower_number(number: &Number) -> PhysicalValue<'static> {
    match number.n {
        N::NegInt(i) => PhysicalValue::Int(i),
        N::Float(f) => PhysicalValue::Float(f.into_inner()),
    }
}

fn lower_text(text: Option<&str>) -> PhysicalValue<'_> {
    text.map_or(PhysicalValue::Null, |text| {
        PhysicalValue::String(Cow::Borrowed(text))
    })
}

/// A vertex as untyped GraphSON 3: `{"id", "label", "properties"}`; `"labels"` follows `"label"`
/// only for a vertex with more than one label.
fn lower_vertex(vertex: &VertexValue) -> PhysicalValue<'_> {
    let mut fields = Vec::with_capacity(4);
    fields.push((
        Cow::Borrowed("id"),
        PhysicalValue::String(Cow::Borrowed(vertex.id())),
    ));
    fields.push((Cow::Borrowed("label"), lower_text(vertex.label())));
    if vertex.labels().len() > 1 {
        fields.push((
            Cow::Borrowed("labels"),
            PhysicalValue::Array(
                vertex
                    .labels()
                    .iter()
                    .map(|label| PhysicalValue::String(Cow::Borrowed(label.as_str())))
                    .collect(),
            ),
        ));
    }
    fields.push((Cow::Borrowed("properties"), lower_map(vertex.properties())));
    PhysicalValue::Object(fields)
}

/// An edge as untyped GraphSON 3: `{"id", "label", "inVLabel", "outVLabel", "inV", "outV",
/// "properties"}`.
fn lower_edge(edge: &EdgeValue) -> PhysicalValue<'_> {
    PhysicalValue::Object(vec![
        (
            Cow::Borrowed("id"),
            PhysicalValue::String(Cow::Borrowed(edge.id())),
        ),
        (Cow::Borrowed("label"), lower_text(edge.label())),
        (Cow::Borrowed("inVLabel"), lower_text(edge.in_v().label())),
        (Cow::Borrowed("outVLabel"), lower_text(edge.out_v().label())),
        (
            Cow::Borrowed("inV"),
            PhysicalValue::String(Cow::Borrowed(edge.in_v().id())),
        ),
        (
            Cow::Borrowed("outV"),
            PhysicalValue::String(Cow::Borrowed(edge.out_v().id())),
        ),
        (Cow::Borrowed("properties"), lower_map(edge.properties())),
    ])
}

fn lower_map(map: &PropertyMap) -> PhysicalValue<'_> {
    PhysicalValue::Object(
        map.iter()
            .map(|(key, value)| (Cow::Borrowed(key.as_str()), lower(value)))
            .collect(),
    )
}

/// Lifts a physical value to a logical one.
///
/// Without a declared type this is [`lift_untyped`]; with one it is
/// `lift_declared(lift_untyped(value), declared)`.
#[cfg_attr(not(test), allow(dead_code))] // The codec's documented composition; pinned by tests.
pub(crate) fn lift(
    value: PhysicalValue<'_>,
    declared: Option<&PropertySchema>,
) -> Result<ElementProperty, LiftError> {
    let untyped = lift_untyped(value);
    match declared {
        Some(declared) => lift_declared(untyped, declared),
        None => Ok(untyped),
    }
}

/// Maps every physical type to the logical type of the same name.
///
/// A string stays a `String` even when its content is a canonical UUID: content is never sniffed
/// (CLAUDE.md rule 7). Only a declared type turns it into a `Uuid`.
#[cfg_attr(not(any(feature = "io", feature = "serde")), allow(dead_code))] // GraphML import and serde.
pub(crate) fn lift_untyped(value: PhysicalValue<'_>) -> ElementProperty {
    match value {
        PhysicalValue::Null => ElementProperty::Null,
        PhysicalValue::Bool(b) => ElementProperty::Boolean(b),
        PhysicalValue::Int(i) => ElementProperty::Number(Number::from_i64(i)),
        PhysicalValue::Float(f) => ElementProperty::Number(Number::from_f64(f)),
        PhysicalValue::String(s) => ElementProperty::String(s.as_ref().into()),
        PhysicalValue::Array(items) => {
            ElementProperty::Array(items.into_iter().map(lift_untyped).collect())
        }
        PhysicalValue::Object(fields) => {
            let mut map = PropertyMap::with_capacity(fields.len());
            for (key, field) in fields {
                map.insert(key.as_ref(), lift_untyped(field));
            }
            ElementProperty::Object(Box::new(map))
        }
    }
}

/// Converts an already same-name-lifted value to its declared type, then checks conformance.
///
/// The conversion is the schema's write coercion (`coerce_to_declared`), and the check is the
/// validator's own type relation (`SchemaValidator::conforms`), so reading a value through the
/// codec accepts exactly what writing it through a schema accepts.
///
/// A top-level `Null` is returned unchanged under any declared type: whether `null` is allowed
/// is the schema mode's business, never the codec's. Only the types are checked (`type`,
/// `format`, `items`, `properties`, `anyOf`); required keys and constraints are enforced by the
/// schema mode when the value is stored.
#[cfg_attr(not(feature = "io"), allow(dead_code))] // Consumed by GraphML import (`io`).
pub(crate) fn lift_declared(
    value: ElementProperty,
    declared: &PropertySchema,
) -> Result<ElementProperty, LiftError> {
    if matches!(value, ElementProperty::Null) {
        return Ok(value);
    }
    let coerced = coerce_to_declared(value, declared).map_err(|failure| LiftError::Coercion {
        declared: declared.non_null_type_name(),
        reason: failure.reason,
    })?;
    if conforms_to(declared, &coerced) {
        Ok(coerced)
    } else {
        Err(LiftError::TypeMismatch {
            declared: declared.non_null_type_name(),
            actual: value_type_name(&coerced),
        })
    }
}

/// Lifts the physical form of a value declared as a temporal type: its text parsed as that type
/// (the inverse of [`lower`] for a temporal value). Any other physical value, or text that is not
/// a value of the type, is an error; nothing else is ever turned into a temporal value (rule 7).
pub(crate) fn lift_temporal(
    value: &PhysicalValue<'_>,
    kind: crate::value::temporal::TemporalKind,
) -> Result<ElementProperty, crate::value::temporal::TemporalError> {
    match value {
        PhysicalValue::String(text) => crate::value::temporal::Temporal::parse_strict(kind, text)
            .map(ElementProperty::Temporal),
        other => {
            let shown = match other {
                PhysicalValue::Null => "null",
                PhysicalValue::Bool(_) => "a boolean",
                PhysicalValue::Int(_) | PhysicalValue::Float(_) => "a number",
                PhysicalValue::Array(_) => "an array",
                PhysicalValue::Object(_) => "an object",
                PhysicalValue::String(_) => "a string",
            };
            Err(crate::value::temporal::TemporalError::parse(
                shown,
                kind.name(),
                "expected its text",
            ))
        }
    }
}

/// The GraphSON 3 type tag of a double: the typed form of a non-finite float in value JSON.
pub(crate) const DOUBLE_TAG: &str = "g:Double";

/// The GraphSON text of a non-finite float: `"NaN"`, `"Infinity"` or `"-Infinity"` (`None` for a
/// finite one). GraphSON export writes it as the payload of a [`DOUBLE_TAG`], and so does the
/// value JSON of the codec ([`write_json_typed`], [`to_json_text`], `TypedFloats`).
pub(crate) fn non_finite_text(value: f64) -> Option<&'static str> {
    if value.is_nan() {
        Some("NaN")
    } else if value == f64::INFINITY {
        Some("Infinity")
    } else if value == f64::NEG_INFINITY {
        Some("-Infinity")
    } else {
        None
    }
}

/// Writes a physical value as compact RFC 8259 JSON text.
///
/// A float is always written as a JSON number with a fraction or exponent (`1.0`, never `1`), so
/// a reader gets a float back. A non-finite float is an error, never silently `null`: this is the
/// strict writer of formats that embed JSON and read it back untyped (GraphML). Value JSON for a
/// reader (results, `to_json()`, the `json` visualizer) uses [`write_json_typed`].
pub(crate) fn write_json<W: fmt::Write>(
    value: &PhysicalValue<'_>,
    out: &mut W,
) -> Result<(), PhysicalError> {
    write_json_as::<W, false>(value, out)
}

/// Writes a physical value as compact JSON text like [`write_json`], except that a non-finite
/// float is written in GraphSON's typed form `{"@type":"g:Double","@value":"NaN"}` (payloads
/// `"NaN"`, `"Infinity"`, `"-Infinity"`) instead of being an error: JSON has no number for it,
/// and the tag says the value is a float, not the string `"NaN"`. Output only: the untyped
/// readers ([`read_json`], serde) do not interpret the tag (like a `g:Map`).
pub(crate) fn write_json_typed<W: fmt::Write>(
    value: &PhysicalValue<'_>,
    out: &mut W,
) -> Result<(), PhysicalError> {
    write_json_as::<W, true>(value, out)
}

fn write_json_as<W: fmt::Write, const TYPED: bool>(
    value: &PhysicalValue<'_>,
    out: &mut W,
) -> Result<(), PhysicalError> {
    match value {
        PhysicalValue::Null => out.write_str("null")?,
        PhysicalValue::Bool(true) => out.write_str("true")?,
        PhysicalValue::Bool(false) => out.write_str("false")?,
        PhysicalValue::Int(i) => write!(out, "{i}")?,
        PhysicalValue::Float(f) => match non_finite_text(*f) {
            Some(text) if TYPED => {
                write!(out, r#"{{"@type":"{DOUBLE_TAG}","@value":"{text}"}}"#)?;
            }
            _ => write_float(*f, out)?,
        },
        PhysicalValue::String(s) => write_json_string(s, out)?,
        PhysicalValue::Array(items) => {
            out.write_char('[')?;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                write_json_as::<W, TYPED>(item, out)?;
            }
            out.write_char(']')?;
        }
        PhysicalValue::Object(fields) => {
            out.write_char('{')?;
            for (i, (key, field)) in fields.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                write_json_string(key, out)?;
                out.write_char(':')?;
                write_json_as::<W, TYPED>(field, out)?;
            }
            out.write_char('}')?;
        }
    }
    Ok(())
}

/// Renders a logical value as compact JSON text: [`lower`] followed by [`write_json_typed`].
///
/// This is the one converter for a value that must become text (a `Uuid` becomes its canonical
/// string, containers become JSON, a non-finite float `{"@type":"g:Double","@value":"NaN"}`),
/// never its `Debug` rendering.
pub(crate) fn to_json_text(value: &ElementProperty) -> Result<String, PhysicalError> {
    let mut out = String::new();
    write_json_typed(&lower(value), &mut out)?;
    Ok(out)
}

/// Renders a logical value as short display text for a table header, a tree label or a REPL map key:
/// a string unquoted, a number `29`/`2.0`, `null`, `true`, a uuid in canonical form, a vertex `v[<id>]`,
/// an edge `e[<id>]`, a list or map as compact JSON of [`lower`].
///
/// Display only: two different values may print the same text (`"29"` and `29`), so never use it as a key.
pub(crate) fn to_display_text(value: &ElementProperty) -> CompactString {
    match value {
        ElementProperty::Vertex(vertex) => {
            return CompactString::from(format!("v[{}]", vertex.id()));
        }
        ElementProperty::Edge(edge) => {
            return CompactString::from(format!("e[{}]", edge.id()));
        }
        ElementProperty::Number(number) => {
            if let N::Float(f) = number.n
                && !f.into_inner().is_finite()
            {
                return CompactString::from(f.into_inner().to_string());
            }
        }
        _ => {}
    }
    match lower(value) {
        PhysicalValue::String(text) => CompactString::from(text),
        other => {
            let mut out = String::new();
            match write_json(&other, &mut out) {
                Ok(()) => CompactString::from(out),
                Err(err) => CompactString::from(err.to_string()),
            }
        }
    }
}

pub(crate) fn write_float<W: fmt::Write>(value: f64, out: &mut W) -> Result<(), PhysicalError> {
    if !value.is_finite() {
        return Err(PhysicalError::NonFiniteFloat(value));
    }
    // `Display` is Rust's shortest round-trip rendering; it omits the fraction of an integral
    // float, which would read back as an integer.
    let text = value.to_string();
    out.write_str(&text)?;
    if !text.contains(['.', 'e', 'E']) {
        out.write_str(".0")?;
    }
    Ok(())
}

pub(crate) fn write_json_string<W: fmt::Write>(
    value: &str,
    out: &mut W,
) -> Result<(), PhysicalError> {
    out.write_char('"')?;
    for c in value.chars() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32)?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')?;
    Ok(())
}

/// Why [`read_json`] rejected a text.
#[cfg_attr(not(feature = "io"), allow(dead_code))] // Consumed by GraphML import (`io`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JsonReadError {
    /// Byte offset into the text where reading stopped.
    pub(crate) offset: usize,
    /// What was wrong there.
    pub(crate) message: &'static str,
}

impl fmt::Display for JsonReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

/// Reads one RFC 8259 JSON text (the counterpart of [`write_json`]) as a physical value.
///
/// Object members keep their document order; an integer without fraction or exponent must fit
/// `i64` (never silently turned into a float), any other number must be a finite `f64`. Nesting
/// deeper than `max_depth` arrays/objects is an error, so arbitrary input cannot exhaust the
/// stack. It needs no dependency, so GraphML import can parse the JSON text of a declared
/// `array`/`object` property without the `serde` feature.
#[cfg_attr(not(feature = "io"), allow(dead_code))] // Consumed by GraphML import (`io`).
pub(crate) fn read_json(
    text: &str,
    max_depth: usize,
) -> Result<PhysicalValue<'static>, JsonReadError> {
    let mut reader = JsonReader {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        max_depth,
    };
    reader.skip_whitespace();
    let value = reader.value(0)?;
    reader.skip_whitespace();
    if reader.pos < reader.bytes.len() {
        return Err(reader.error("unexpected text after the JSON value"));
    }
    Ok(value)
}

/// The cursor of [`read_json`].
#[cfg_attr(not(feature = "io"), allow(dead_code))]
struct JsonReader<'t> {
    text: &'t str,
    bytes: &'t [u8],
    pos: usize,
    max_depth: usize,
}

#[cfg_attr(not(feature = "io"), allow(dead_code))]
impl JsonReader<'_> {
    fn error(&self, message: &'static str) -> JsonReadError {
        JsonReadError {
            offset: self.pos,
            message,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn literal(&mut self, word: &'static str, value: PhysicalValue<'static>) -> ReadResult {
        if self.text[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn value(&mut self, depth: usize) -> ReadResult {
        match self.peek() {
            None => Err(self.error("unexpected end of the JSON text")),
            Some(b'n') => self.literal("null", PhysicalValue::Null),
            Some(b't') => self.literal("true", PhysicalValue::Bool(true)),
            Some(b'f') => self.literal("false", PhysicalValue::Bool(false)),
            Some(b'"') => Ok(PhysicalValue::String(Cow::Owned(self.string()?))),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => {
                let depth = self.enter(depth)?;
                let mut items = Vec::new();
                self.skip_whitespace();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                    return Ok(PhysicalValue::Array(items));
                }
                loop {
                    self.skip_whitespace();
                    items.push(self.value(depth)?);
                    self.skip_whitespace();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(PhysicalValue::Array(items));
                        }
                        _ => return Err(self.error("expected ',' or ']' in an array")),
                    }
                }
            }
            Some(b'{') => {
                let depth = self.enter(depth)?;
                let mut fields = Vec::new();
                self.skip_whitespace();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    return Ok(PhysicalValue::Object(fields));
                }
                loop {
                    self.skip_whitespace();
                    if self.peek() != Some(b'"') {
                        return Err(self.error("expected a string key in an object"));
                    }
                    let key = self.string()?;
                    self.skip_whitespace();
                    if self.peek() != Some(b':') {
                        return Err(self.error("expected ':' after an object key"));
                    }
                    self.pos += 1;
                    self.skip_whitespace();
                    let field = self.value(depth)?;
                    fields.push((Cow::Owned(key), field));
                    self.skip_whitespace();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(PhysicalValue::Object(fields));
                        }
                        _ => return Err(self.error("expected ',' or '}' in an object")),
                    }
                }
            }
            Some(_) => Err(self.error("unexpected character")),
        }
    }

    /// Consumes the opening bracket of a container and returns the depth inside it.
    fn enter(&mut self, depth: usize) -> Result<usize, JsonReadError> {
        if depth >= self.max_depth {
            return Err(self.error("arrays/objects nested too deeply"));
        }
        self.pos += 1;
        Ok(depth + 1)
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> ReadResult {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let int_start = self.pos;
        let int_digits = self.digits();
        if int_digits == 0 || (int_digits > 1 && self.bytes[int_start] == b'0') {
            return Err(self.error("invalid number"));
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if self.digits() == 0 {
                return Err(self.error("invalid number"));
            }
            is_float = true;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return Err(self.error("invalid number"));
            }
            is_float = true;
        }
        let literal = &self.text[start..self.pos];
        if is_float {
            match literal.parse::<f64>() {
                Ok(f) if f.is_finite() => Ok(PhysicalValue::Float(f)),
                _ => Err(JsonReadError {
                    offset: start,
                    message: "number out of the float64 range",
                }),
            }
        } else {
            literal
                .parse::<i64>()
                .map(PhysicalValue::Int)
                .map_err(|_| JsonReadError {
                    offset: start,
                    message: "integer out of the int64 range",
                })
        }
    }

    /// Reads a string starting at its opening quote.
    fn string(&mut self) -> Result<String, JsonReadError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let run_start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // Stops only at ASCII bytes, so the run ends on a char boundary.
            out.push_str(&self.text[run_start..self.pos]);
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let escaped = match self.peek() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.pos += 1;
                            let c = self.unicode_escape()?;
                            out.push(c);
                            continue;
                        }
                        _ => return Err(self.error("invalid escape in a string")),
                    };
                    self.pos += 1;
                    out.push(escaped);
                }
                Some(_) => return Err(self.error("unescaped control character in a string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonReadError> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| self.error("invalid \\u escape"))?;
        let code = u32::from_str_radix(digits, 16).map_err(|_| self.error("invalid \\u escape"))?;
        self.pos += 4;
        Ok(code)
    }

    /// The character of a `\uXXXX` escape (after the `u`), joining a surrogate pair.
    fn unicode_escape(&mut self) -> Result<char, JsonReadError> {
        let high = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&high) {
            if !self.text[self.pos..].starts_with("\\u") {
                return Err(self.error("unpaired surrogate in a \\u escape"));
            }
            self.pos += 2;
            let low = self.hex4()?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(self.error("unpaired surrogate in a \\u escape"));
            }
            0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
        } else {
            high
        };
        char::from_u32(code).ok_or_else(|| self.error("unpaired surrogate in a \\u escape"))
    }
}

#[cfg_attr(not(feature = "io"), allow(dead_code))]
type ReadResult = Result<PhysicalValue<'static>, JsonReadError>;

#[cfg(feature = "serde")]
pub(crate) use self::serde_glue::from_serde_json;
#[cfg(feature = "serde")]
pub(crate) use self::serde_glue::{Tagged, TaggedOwned, TypedFloats};

/// Plain-JSON serde for [`ElementProperty`] and [`Number`], built on the codec.
#[cfg(feature = "serde")]
mod serde_glue {
    use super::*;
    use serde::de::{self, MapAccess, SeqAccess, Visitor};
    use serde::ser::{self, SerializeMap, SerializeSeq};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::value::temporal::tags;
    use crate::value::vector::tagged as vector_tagged;

    const NON_FINITE: &str = "non-finite float cannot be represented in JSON";

    /// A physical value serialized strictly (`TYPED = false`: a non-finite float is an error) or
    /// as value JSON (`TYPED = true`: a non-finite float is `{"@type": "g:Double", "@value":
    /// "NaN"}`, the form of [`write_json_typed`]).
    struct Physical<'b, 'a, const TYPED: bool>(&'b PhysicalValue<'a>);

    impl<const TYPED: bool> Serialize for Physical<'_, '_, TYPED> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            match self.0 {
                PhysicalValue::Null => serializer.serialize_unit(),
                PhysicalValue::Bool(b) => serializer.serialize_bool(*b),
                PhysicalValue::Int(i) => serializer.serialize_i64(*i),
                PhysicalValue::Float(f) => match non_finite_text(*f) {
                    None => serializer.serialize_f64(*f),
                    Some(text) if TYPED => {
                        let mut map = serializer.serialize_map(Some(2))?;
                        map.serialize_entry(TYPE_KEY, DOUBLE_TAG)?;
                        map.serialize_entry(VALUE_KEY, text)?;
                        map.end()
                    }
                    Some(_) => Err(ser::Error::custom(NON_FINITE)),
                },
                PhysicalValue::String(s) => serializer.serialize_str(s),
                PhysicalValue::Array(items) => {
                    let mut seq = serializer.serialize_seq(Some(items.len()))?;
                    for item in items {
                        seq.serialize_element(&Physical::<TYPED>(item))?;
                    }
                    seq.end()
                }
                PhysicalValue::Object(fields) => {
                    let mut map = serializer.serialize_map(Some(fields.len()))?;
                    for (key, field) in fields {
                        map.serialize_entry(key.as_ref(), &Physical::<TYPED>(field))?;
                    }
                    map.end()
                }
            }
        }
    }

    impl Serialize for PhysicalValue<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            Physical::<false>(self).serialize(serializer)
        }
    }

    /// Logical values serialized as value JSON: a list of [`lower`]ed values, each non-finite
    /// float in GraphSON's typed form `{"@type": "g:Double", "@value": "NaN"}` (`"Infinity"`,
    /// `"-Infinity"`), exactly the text of [`to_json_text`]. The `json` visualizer writes its
    /// results through it; the plain serde of [`ElementProperty`] stays strict.
    pub(crate) struct TypedFloats<'a>(pub(crate) &'a [ElementProperty]);

    impl Serialize for TypedFloats<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
            for item in self.0 {
                seq.serialize_element(&Physical::<true>(&lower(item)))?;
            }
            seq.end()
        }
    }

    /// An owned physical value read by serde.
    struct OwnedPhysical(PhysicalValue<'static>);

    struct PhysicalVisitor;

    impl<'de> Visitor<'de> for PhysicalVisitor {
        type Value = OwnedPhysical;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a JSON value")
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(OwnedPhysical(PhysicalValue::Null))
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(OwnedPhysical(PhysicalValue::Null))
        }

        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            OwnedPhysical::deserialize(d)
        }

        fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
            Ok(OwnedPhysical(PhysicalValue::Bool(v)))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(OwnedPhysical(PhysicalValue::Int(v)))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            i64::try_from(v)
                .map(|i| OwnedPhysical(PhysicalValue::Int(i)))
                .map_err(|_| E::custom(PhysicalError::IntegerOutOfRange(v)))
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
            Ok(OwnedPhysical(PhysicalValue::Float(v)))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(OwnedPhysical(PhysicalValue::String(Cow::Owned(
                v.to_owned(),
            ))))
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(OwnedPhysical(PhysicalValue::String(Cow::Owned(v))))
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
            while let Some(OwnedPhysical(item)) = seq.next_element()? {
                items.push(item);
            }
            Ok(OwnedPhysical(PhysicalValue::Array(items)))
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut fields = Vec::with_capacity(map.size_hint().unwrap_or(0));
            while let Some((key, OwnedPhysical(field))) =
                map.next_entry::<String, OwnedPhysical>()?
            {
                fields.push((Cow::Owned(key), field));
            }
            Ok(OwnedPhysical(PhysicalValue::Object(fields)))
        }
    }

    impl<'de> Deserialize<'de> for OwnedPhysical {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_any(PhysicalVisitor)
        }
    }

    /// Serializes as plain JSON through `lower`; a `Uuid` becomes its canonical string.
    impl Serialize for ElementProperty {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            lower(self).serialize(serializer)
        }
    }

    /// Deserializes plain JSON through `lift_untyped`: every JSON type becomes the logical
    /// type of the same name, so a UUID string comes back as a `String`.
    impl<'de> Deserialize<'de> for ElementProperty {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let OwnedPhysical(physical) = OwnedPhysical::deserialize(deserializer)?;
            Ok(lift_untyped(physical))
        }
    }

    /// Serializes as a plain JSON number.
    impl Serialize for Number {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            lower_number(self).serialize(serializer)
        }
    }

    struct NumberVisitor;

    impl<'de> Visitor<'de> for NumberVisitor {
        type Value = Number;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a JSON number")
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Number, E> {
            Ok(Number::from_i64(v))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Number, E> {
            i64::try_from(v)
                .map(Number::from_i64)
                .map_err(|_| E::custom(PhysicalError::IntegerOutOfRange(v)))
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Number, E> {
            Ok(Number::from_f64(v))
        }
    }

    /// Deserializes a plain JSON number: an integer becomes an `int64`, any other number a
    /// `float64`.
    impl<'de> Deserialize<'de> for Number {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_any(NumberVisitor)
        }
    }

    /// The key of a per-value type tag (TinkerPop GraphSON style).
    pub(crate) const TYPE_KEY: &str = "@type";
    /// The key of a tagged value's payload.
    pub(crate) const VALUE_KEY: &str = "@value";
    /// The tag of a `uuid`: `{"@type": "g:UUID", "@value": "<canonical>"}` (GraphSON's name).
    pub(crate) const UUID_TAG: &str = "g:UUID";
    /// The tag that escapes a user object whose own keys include `"@type"`:
    /// `{"@type": "graphersal:Object", "@value": {..the object..}}`. Its payload's keys are
    /// taken literally (the payload itself is never read as a tag; its values are).
    pub(crate) const OBJECT_TAG: &str = "graphersal:Object";

    /// The *tagged* JSON form of a value (rule 7: an explicit per-value type tag): JSON-native
    /// types are plain JSON, a `uuid` is `{"@type": "g:UUID", "@value": "<canonical>"}` at any
    /// depth, a temporal value is `{"@type": "gx:OffsetDateTime", "@value": "<canonical>"}` (the
    /// tags of `temporal::tags`), and an object that has an `"@type"` key of its own is escaped with
    /// [`OBJECT_TAG`], so no user map can be misread as a tag. [`TaggedOwned`] reads it back
    /// exactly. Used where a value must survive a round trip without a schema (the serialized
    /// `ChangeSet`); everything else keeps the plain codec.
    pub(crate) struct Tagged<'a>(pub(crate) &'a ElementProperty);

    /// The entries of a property map in the tagged form (no escape at this level).
    struct TaggedEntries<'a>(&'a PropertyMap);

    impl Serialize for TaggedEntries<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (key, value) in self.0 {
                map.serialize_entry(key.as_str(), &Tagged(value))?;
            }
            map.end()
        }
    }

    impl Serialize for Tagged<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            match self.0 {
                ElementProperty::Uuid(value) => {
                    let mut map = serializer.serialize_map(Some(2))?;
                    map.serialize_entry(TYPE_KEY, UUID_TAG)?;
                    map.serialize_entry(VALUE_KEY, format_canonical(*value).as_str())?;
                    map.end()
                }
                ElementProperty::Array(items) => {
                    let mut seq = serializer.serialize_seq(Some(items.len()))?;
                    for item in items {
                        seq.serialize_element(&Tagged(item))?;
                    }
                    seq.end()
                }
                ElementProperty::Object(fields) if fields.contains_key(TYPE_KEY) => {
                    let mut map = serializer.serialize_map(Some(2))?;
                    map.serialize_entry(TYPE_KEY, OBJECT_TAG)?;
                    map.serialize_entry(VALUE_KEY, &TaggedEntries(fields))?;
                    map.end()
                }
                ElementProperty::Object(fields) => TaggedEntries(fields).serialize(serializer),
                // `{"@type": "gx:OffsetDateTime", "@value": "<canonical text>"}`, ...
                // (`temporal::tags`).
                ElementProperty::Temporal(value) => {
                    let mut map = serializer.serialize_map(Some(2))?;
                    map.serialize_entry(TYPE_KEY, tags::tag_of(value))?;
                    map.serialize_entry(VALUE_KEY, &value.to_string())?;
                    map.end()
                }
                // `{"@type": "graphersal:Vector", "@value": {"element": .., ..}}` (exact,
                // `vector::tagged`).
                ElementProperty::Vector(value) => {
                    let mut map = serializer.serialize_map(Some(2))?;
                    map.serialize_entry(TYPE_KEY, vector_tagged::VECTOR_TAG)?;
                    map.serialize_entry(VALUE_KEY, &vector_tagged::payload(value))?;
                    map.end()
                }
                // Scalars are JSON-native. Vertices, edges and non-string-keyed maps are never
                // property values; they keep the plain codec.
                ElementProperty::Null
                | ElementProperty::String(_)
                | ElementProperty::Compressed(_)
                | ElementProperty::Deduped(_)
                | ElementProperty::Number(_)
                | ElementProperty::Boolean(_)
                | ElementProperty::Vertex(_)
                | ElementProperty::Edge(_)
                | ElementProperty::Map(_) => lower(self.0).serialize(serializer),
            }
        }
    }

    /// A value read from the tagged JSON form written by [`Tagged`]: a `g:UUID` tag becomes a
    /// `Uuid`, a temporal tag (`gx:OffsetDateTime`, ...) its temporal value, a
    /// `graphersal:Object` tag the escaped object, every other JSON type the logical type of the
    /// same name. An object with an `"@type"` key that is not exactly one of these tags (`@type`
    /// and `@value` only, a canonical uuid string, a temporal text of the tag's type, an object
    /// payload) is an error: it cannot have been written by [`Tagged`], and guessing would
    /// corrupt data.
    pub(crate) struct TaggedOwned(pub(crate) ElementProperty);

    impl<'de> Deserialize<'de> for TaggedOwned {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let OwnedPhysical(physical) = OwnedPhysical::deserialize(deserializer)?;
            lift_tagged(physical)
                .map(TaggedOwned)
                .map_err(de::Error::custom)
        }
    }

    /// See [`TaggedOwned`].
    fn lift_tagged(value: PhysicalValue<'_>) -> Result<ElementProperty, String> {
        match value {
            PhysicalValue::Array(items) => Ok(ElementProperty::Array(
                items
                    .into_iter()
                    .map(lift_tagged)
                    .collect::<Result<_, _>>()?,
            )),
            PhysicalValue::Object(fields) if fields.iter().any(|(k, _)| k == TYPE_KEY) => {
                lift_tag(fields)
            }
            PhysicalValue::Object(fields) => lift_tagged_entries(fields),
            other => Ok(lift_untyped(other)),
        }
    }

    fn lift_tagged_entries(
        fields: Vec<(Cow<'_, str>, PhysicalValue<'_>)>,
    ) -> Result<ElementProperty, String> {
        let mut map = PropertyMap::with_capacity(fields.len());
        for (key, field) in fields {
            map.insert(key.as_ref(), lift_tagged(field)?);
        }
        Ok(ElementProperty::Object(Box::new(map)))
    }

    /// An object carrying `"@type"`: exactly `{"@type": <tag>, "@value": <payload>}`.
    fn lift_tag(fields: Vec<(Cow<'_, str>, PhysicalValue<'_>)>) -> Result<ElementProperty, String> {
        let mut tag = None;
        let mut payload = None;
        for (key, field) in fields {
            match key.as_ref() {
                TYPE_KEY if tag.is_none() => tag = Some(field),
                VALUE_KEY if payload.is_none() => payload = Some(field),
                other => {
                    return Err(format!(
                        "a value tagged with \"{TYPE_KEY}\" may only have the keys \"{TYPE_KEY}\" and \"{VALUE_KEY}\", found \"{other}\""
                    ));
                }
            }
        }
        match (tag, payload) {
            (Some(PhysicalValue::String(tag)), Some(PhysicalValue::String(text)))
                if tag == UUID_TAG =>
            {
                crate::value::uuid::parse_canonical(&text)
                    .map(ElementProperty::Uuid)
                    .ok_or_else(|| format!("\"{text}\" is not a canonical uuid ({UUID_TAG})"))
            }
            (Some(PhysicalValue::String(tag)), Some(PhysicalValue::Object(fields)))
                if tag == OBJECT_TAG =>
            {
                lift_tagged_entries(fields)
            }
            (Some(PhysicalValue::String(tag)), Some(payload))
                if tag == vector_tagged::VECTOR_TAG =>
            {
                vector_tagged::from_payload(&payload).map(ElementProperty::Vector)
            }
            (Some(PhysicalValue::String(tag)), Some(PhysicalValue::String(text)))
                if tags::is_tag(&tag) =>
            {
                match tags::read_tagged(&tag, &text) {
                    Some(Ok(value)) => Ok(ElementProperty::Temporal(value)),
                    Some(Err(err)) => Err(format!("{tag}: {err}")),
                    None => Err(format!("unknown type tag \"{tag}\"")),
                }
            }
            (Some(PhysicalValue::String(tag)), Some(_)) => Err(format!(
                "unknown or malformed type tag \"{tag}\" (known: \"{UUID_TAG}\" with a uuid string, \"{OBJECT_TAG}\" with an object, the temporal tags \"gx:OffsetDateTime\", \"gx:LocalDate\", \"gx:LocalDateTime\", \"gx:LocalTime\", \"gx:Duration\", \"gx:Period\", \"graphersal:Interval\" with their text, \"graphersal:Vector\" with its object)"
            )),
            _ => Err(format!(
                "a tagged value needs a string \"{TYPE_KEY}\" and a \"{VALUE_KEY}\""
            )),
        }
    }

    /// Borrows a `serde_json::Value` as a physical value.
    ///
    /// An integer above `i64::MAX` is rejected instead of being silently downcast to a float.
    pub(crate) fn from_serde_json(
        value: &serde_json::Value,
    ) -> Result<PhysicalValue<'_>, PhysicalError> {
        use serde_json::Value;
        Ok(match value {
            Value::Null => PhysicalValue::Null,
            Value::Bool(b) => PhysicalValue::Bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    PhysicalValue::Int(i)
                } else if let Some(u) = n.as_u64() {
                    return Err(PhysicalError::IntegerOutOfRange(u));
                } else if let Some(f) = n.as_f64() {
                    PhysicalValue::Float(f)
                } else {
                    // Unreachable with serde_json's number model; kept total without panicking.
                    return Err(PhysicalError::NonFiniteFloat(f64::NAN));
                }
            }
            Value::String(s) => PhysicalValue::String(Cow::Borrowed(s.as_str())),
            Value::Array(items) => PhysicalValue::Array(
                items
                    .iter()
                    .map(from_serde_json)
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(fields) => PhysicalValue::Object(
                fields
                    .iter()
                    .map(|(k, v)| Ok((Cow::Borrowed(k.as_str()), from_serde_json(v)?)))
                    .collect::<Result<_, PhysicalError>>()?,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::schema::types::type_of;

    const UUID_TEXT: &str = "123e4567-e89b-12d3-a456-426614174000";

    fn uuid_value() -> ElementProperty {
        ElementProperty::Uuid(crate::value::uuid::parse_canonical(UUID_TEXT).unwrap())
    }

    fn object(fields: Vec<(&str, ElementProperty)>) -> ElementProperty {
        let mut map = PropertyMap::default();
        for (k, v) in fields {
            map.insert(k, v);
        }
        ElementProperty::Object(Box::new(map))
    }

    fn pstr(s: &str) -> PhysicalValue<'static> {
        PhysicalValue::String(Cow::Owned(s.to_owned()))
    }

    fn to_json(value: &PhysicalValue<'_>) -> Result<String, PhysicalError> {
        let mut out = String::new();
        write_json(value, &mut out)?;
        Ok(out)
    }

    #[test]
    fn lower_leaves() {
        assert_eq!(lower(&ElementProperty::Null), PhysicalValue::Null);
        assert_eq!(
            lower(&ElementProperty::Boolean(true)),
            PhysicalValue::Bool(true)
        );
        assert_eq!(
            lower(&ElementProperty::Number(7i64.into())),
            PhysicalValue::Int(7)
        );
        assert_eq!(
            lower(&ElementProperty::Number(1.5.into())),
            PhysicalValue::Float(1.5)
        );
        assert_eq!(lower(&ElementProperty::String("a".into())), pstr("a"));
        assert_eq!(lower(&uuid_value()), pstr(UUID_TEXT));
    }

    #[test]
    fn lower_nested_containers_and_elements() {
        let value = object(vec![
            ("id", uuid_value()),
            (
                "tags",
                ElementProperty::Array(vec![ElementProperty::Number(1i64.into())]),
            ),
        ]);
        let expected = PhysicalValue::Object(vec![
            (Cow::Borrowed("id"), pstr(UUID_TEXT)),
            (
                Cow::Borrowed("tags"),
                PhysicalValue::Array(vec![PhysicalValue::Int(1)]),
            ),
        ]);
        assert_eq!(lower(&value), expected);

        let mut map = PropertyMap::default();
        map.insert("name", ElementProperty::String("marko".into()));
        let vertex =
            ElementProperty::Vertex(Box::new(VertexValue::new("1", vec!["person".into()], map)));
        assert_eq!(
            lower(&vertex),
            PhysicalValue::Object(vec![
                (Cow::Borrowed("id"), pstr("1")),
                (Cow::Borrowed("label"), pstr("person")),
                (
                    Cow::Borrowed("properties"),
                    PhysicalValue::Object(vec![(Cow::Borrowed("name"), pstr("marko"))])
                ),
            ])
        );
    }

    #[test]
    fn lift_untyped_maps_same_name() {
        assert_eq!(lift(PhysicalValue::Null, None), Ok(ElementProperty::Null));
        assert_eq!(
            lift(PhysicalValue::Bool(false), None),
            Ok(ElementProperty::Boolean(false))
        );
        assert_eq!(
            lift(PhysicalValue::Int(3), None),
            Ok(ElementProperty::Number(3i64.into()))
        );
        assert_eq!(
            lift(PhysicalValue::Float(2.5), None),
            Ok(ElementProperty::Number(2.5.into()))
        );
        // A canonical UUID string is not sniffed.
        assert_eq!(
            lift(pstr(UUID_TEXT), None),
            Ok(ElementProperty::String(UUID_TEXT.into()))
        );
        assert_eq!(
            lift(PhysicalValue::Array(vec![PhysicalValue::Int(1)]), None),
            Ok(ElementProperty::Array(vec![ElementProperty::Number(
                1i64.into()
            )]))
        );
        assert_eq!(
            lift(
                PhysicalValue::Object(vec![(Cow::Borrowed("k"), pstr("v"))]),
                None
            ),
            Ok(object(vec![("k", ElementProperty::String("v".into()))]))
        );
    }

    #[test]
    fn lift_declared_uuid() {
        assert_eq!(
            lift(pstr(UUID_TEXT), Some(&PropertySchema::uuid())),
            Ok(uuid_value())
        );
        assert!(matches!(
            lift(pstr("ABC"), Some(&PropertySchema::uuid())),
            Err(LiftError::Coercion { ref declared, .. }) if declared == "uuid"
        ));
    }

    #[test]
    fn lift_declared_numbers() {
        assert_eq!(
            lift(PhysicalValue::Int(3), Some(&PropertySchema::number())),
            Ok(ElementProperty::Number(Number::from_f64(3.0)))
        );
        assert_eq!(
            lift(PhysicalValue::Float(1.5), Some(&PropertySchema::integer())),
            Err(LiftError::TypeMismatch {
                declared: "int64".to_string(),
                actual: "float64".to_string()
            })
        );
    }

    #[test]
    fn lift_declared_null_passes_through() {
        assert_eq!(
            lift(PhysicalValue::Null, Some(&PropertySchema::integer())),
            Ok(ElementProperty::Null)
        );
    }

    #[test]
    fn lift_declared_nested_object() {
        let declared = PropertySchema::object()
            .with_property("id", PropertySchema::uuid())
            .with_property("ids", PropertySchema::array(PropertySchema::uuid()));
        let physical = PhysicalValue::Object(vec![
            (Cow::Borrowed("id"), pstr(UUID_TEXT)),
            (
                Cow::Borrowed("ids"),
                PhysicalValue::Array(vec![pstr(UUID_TEXT)]),
            ),
        ]);
        assert_eq!(
            lift(physical, Some(&declared)),
            Ok(object(vec![
                ("id", uuid_value()),
                ("ids", ElementProperty::Array(vec![uuid_value()])),
            ]))
        );
    }

    #[test]
    fn write_json_escapes_and_orders() {
        let value = PhysicalValue::Object(vec![
            (Cow::Borrowed("z"), pstr("a\"b\\c\nd\re\tf\u{1}g")),
            (
                Cow::Borrowed("a"),
                PhysicalValue::Array(vec![
                    PhysicalValue::Null,
                    PhysicalValue::Bool(true),
                    PhysicalValue::Int(-4),
                ]),
            ),
        ]);
        assert_eq!(
            to_json(&value).unwrap(),
            r#"{"z":"a\"b\\c\nd\re\tf\u0001g","a":[null,true,-4]}"#
        );
    }

    #[test]
    fn write_json_floats() {
        assert_eq!(to_json(&PhysicalValue::Float(1.0)).unwrap(), "1.0");
        assert_eq!(to_json(&PhysicalValue::Float(-2.5)).unwrap(), "-2.5");
        assert_eq!(to_json(&PhysicalValue::Float(0.1)).unwrap(), "0.1");
        assert!(matches!(
            to_json(&PhysicalValue::Float(f64::NAN)),
            Err(PhysicalError::NonFiniteFloat(_))
        ));
        assert!(matches!(
            to_json(&PhysicalValue::Float(f64::INFINITY)),
            Err(PhysicalError::NonFiniteFloat(_))
        ));
    }

    fn float(f: f64) -> ElementProperty {
        ElementProperty::Number(Number::from_f64(f))
    }

    #[test]
    fn value_json_writes_non_finite_floats_as_typed_doubles() {
        assert_eq!(non_finite_text(f64::NAN), Some("NaN"));
        assert_eq!(non_finite_text(f64::INFINITY), Some("Infinity"));
        assert_eq!(non_finite_text(f64::NEG_INFINITY), Some("-Infinity"));
        assert_eq!(non_finite_text(1.5), None);
        assert_eq!(
            to_json_text(&float(f64::NAN)).unwrap(),
            r#"{"@type":"g:Double","@value":"NaN"}"#
        );
        assert_eq!(
            to_json_text(&float(f64::INFINITY)).unwrap(),
            r#"{"@type":"g:Double","@value":"Infinity"}"#
        );
        // At any depth, finite floats unchanged.
        let nested = object(vec![
            (
                "xs",
                ElementProperty::Array(vec![float(f64::NEG_INFINITY), float(2.0)]),
            ),
            ("s", ElementProperty::String("NaN".into())),
        ]);
        assert_eq!(
            to_json_text(&nested).unwrap(),
            r#"{"xs":[{"@type":"g:Double","@value":"-Infinity"},2.0],"s":"NaN"}"#
        );
        // The strict writer keeps refusing it (GraphML embeds JSON that is read back untyped).
        assert!(matches!(
            write_json(&lower(&nested), &mut String::new()),
            Err(PhysicalError::NonFiniteFloat(_))
        ));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn typed_floats_serde_matches_to_json_text() {
        let values = vec![
            float(f64::NAN),
            ElementProperty::Array(vec![float(f64::INFINITY), float(0.5)]),
            ElementProperty::Number(Number::from_i64(3)),
        ];
        let text = serde_json::to_string(&TypedFloats(&values)).unwrap();
        let expected: Vec<String> = values.iter().map(|v| to_json_text(v).unwrap()).collect();
        assert_eq!(text, format!("[{}]", expected.join(",")));
        // The plain serde of a value stays strict.
        assert!(serde_json::to_string(&float(f64::NAN)).is_err());
    }

    #[test]
    fn round_trip_through_declared_type() {
        let values = vec![
            ElementProperty::Null,
            ElementProperty::Boolean(true),
            ElementProperty::Number(42i64.into()),
            ElementProperty::Number(Number::from_f64(3.0)),
            ElementProperty::Number(0.25.into()),
            ElementProperty::String("text".into()),
            uuid_value(),
            ElementProperty::Array(vec![uuid_value(), uuid_value()]),
            object(vec![
                ("id", uuid_value()),
                ("n", ElementProperty::Number(Number::from_f64(1.0))),
                (
                    "inner",
                    object(vec![("s", ElementProperty::String("x".into()))]),
                ),
            ]),
        ];
        for value in values {
            // The declared node is what inference reports for the value.
            let ty = type_of(&value).unwrap();
            let mut inferer = crate::graph::schema::SchemaInferer::new();
            let mut properties = crate::value::PropertyMap::default();
            properties.insert("v", value.clone());
            inferer.observe_vertex(&["x".into()], &properties);
            let schema = inferer.finish();
            let declared = &schema.vertices["x"].schema.properties["v"];
            assert_eq!(
                lift(lower(&value), Some(declared)),
                Ok(value.clone()),
                "{ty}"
            );
        }
        // Without a declaration a UUID comes back as its string, as designed.
        assert_eq!(
            lift(lower(&uuid_value()), None),
            Ok(ElementProperty::String(UUID_TEXT.into()))
        );
    }

    /// `read_json` reads back everything `write_json` writes, in member order, with exact types.
    #[test]
    fn read_json_round_trips_write_json() {
        let value = PhysicalValue::Object(vec![
            (Cow::Borrowed("z"), PhysicalValue::Int(-7)),
            (Cow::Borrowed("a"), PhysicalValue::Float(1.0)),
            (
                Cow::Borrowed("s"),
                PhysicalValue::String(Cow::Borrowed("q\"\\/\u{1}\n\u{1F600}é")),
            ),
            (
                Cow::Borrowed("list"),
                PhysicalValue::Array(vec![
                    PhysicalValue::Null,
                    PhysicalValue::Bool(true),
                    PhysicalValue::Bool(false),
                    PhysicalValue::Array(vec![]),
                    PhysicalValue::Object(vec![]),
                    PhysicalValue::Float(-2.5e-3),
                ]),
            ),
        ]);
        let mut text = String::new();
        write_json(&value, &mut text).unwrap();
        assert_eq!(read_json(&text, 8), Ok(value));
    }

    #[test]
    fn read_json_accepts_whitespace_escapes_and_surrogate_pairs() {
        assert_eq!(
            read_json(" [ 1 , \"\\ud83d\\ude00\\u00e9\" ]\n", 4),
            Ok(PhysicalValue::Array(vec![
                PhysicalValue::Int(1),
                PhysicalValue::String(Cow::Borrowed("\u{1F600}é")),
            ]))
        );
        assert_eq!(read_json("-0", 1), Ok(PhysicalValue::Int(0)));
        assert_eq!(read_json("1E2", 1), Ok(PhysicalValue::Float(100.0)));
    }

    #[test]
    fn read_json_rejects_malformed_text() {
        for bad in [
            "",
            "nul",
            "[1,]",
            "{\"a\" 1}",
            "{a: 1}",
            "01",
            "1.",
            "-",
            "1e",
            "\"open",
            "\"\\x\"",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"tab\there\"",
            "[1] 2",
            "9223372036854775808",
            "1e999",
            "'a'",
        ] {
            assert!(read_json(bad, 8).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn read_json_bounds_the_nesting_depth() {
        assert!(read_json("[[1]]", 2).is_ok());
        let err = read_json("[[[1]]]", 2).unwrap_err();
        assert_eq!(err.message, "arrays/objects nested too deeply");
        assert_eq!(err.offset, 2);
        // Deep input fails fast instead of exhausting the stack.
        let deep = "[".repeat(100_000);
        assert!(read_json(&deep, 128).is_err());
    }

    #[test]
    fn encoding_of_leaves_and_containers() {
        assert_eq!(encoding_of(&ValueType::Uuid), Some(Encoding::String));
        assert_eq!(encoding_of(&ValueType::Int64), Some(Encoding::Int64));
        assert_eq!(
            encoding_of(&ValueType::Array(Box::new(ValueType::Int64))),
            None
        );
    }
}
