//! GraphSON 3.0 import and export (TinkerPop's adjacency-list graph format).
//!
//! **Import** reads the line form (one vertex object per line, what TinkerPop's `io()` step and
//! `GraphSONWriter` write by default) and the wrapped form (`{"vertices": [...]}`). One lenient
//! value decoder reads GraphSON 3.0 typed values, GraphSON 3.0 `types=false`, GraphSON 2.0 and
//! untyped GraphSON 1.0. The import never guesses: a value whose type Graphersal cannot hold is
//! skipped with an entry in the returned [`ImportReport`], never converted to a string or a
//! number. The `java.time` types map to the temporal types (`gx:OffsetDateTime` -> `datetime`,
//! ...); the date types Graphersal cannot hold (`gx:Year`, `gx:MonthDay`, ...) are skipped.
//!
//! **Export** writes the line form with GraphSON 3.0 typed values, which TinkerGraph's
//! `g.io(file).read()` loads: both `inE` and `outE` on every vertex, unique vertex-property ids,
//! only core `g:` types.
//!
//! Mappings (import -> Graphersal -> export):
//!
//! | GraphSON | Graphersal | Export |
//! |---|---|---|
//! | string, boolean, null | `string`, `boolean`, `null` | same |
//! | `g:Int32`, `g:Int64`, `gx:Int16`, `gx:Byte`, `gx:BigInteger` (within int64), plain integer | `int64` | `g:Int64` |
//! | `g:Double`, `g:Float`, plain decimal (`"NaN"`, `"Infinity"`, `"-Infinity"` payloads too) | `float64` | `g:Double` |
//! | `g:UUID` | `uuid` | `g:UUID` |
//! | `g:List`, `g:Set`, plain array | `array` | `g:List` |
//! | `g:Map` with string keys, plain object | `object` | `g:Map` |
//! | `gx:Char` | `string` | string |
//! | `g:Date`, `g:Timestamp` (epoch ms), `gx:Instant`, `gx:OffsetDateTime`, `gx:ZonedDateTime` (zone id dropped, reported) | `datetime` | `gx:OffsetDateTime` |
//! | `gx:LocalDate`, `gx:LocalDateTime`, `gx:LocalTime` | `date`, `localdatetime`, `time` | the same tags |
//! | `gx:Duration` (a day = 24 h), `gx:Period` | `duration` | `gx:Duration` (time part only), `gx:Period` (months and days only), else `graphersal:Interval` |
//! | `g:List` of numbers where the target schema declares a vector (mode `open`/`closed`: coerced on write); `graphersal:Vector` (exact, our own tag) | `vector` | `g:List` of `g:Float` (an i8 or bit vector as its f32 values) |
//! | `gx:Year`, `gx:YearMonth`, `gx:MonthDay`, `gx:OffsetTime`, `gx:ZoneOffset`, other types | skipped, reported | - |
//!
//! Structure: element ids become strings (`g:Int32 1` -> `"1"`), a label `"A::B"` becomes the
//! label set `[A, B]`, a vertex property with several entries becomes one array, meta-properties
//! go into the reserved `_meta` object property (`_meta.k = {..}` for one entry, an aligned array
//! of objects for several), and an export restores them. GraphSON carries no schema: set the
//! schema on the target graph first.
//!
//! Deliberate deviations from TinkerPop's reader and writer (book: `users_guide/graphson.md`,
//! `for_developers/tinkerpop_deviations.md`): ids are strings (TinkerGraph then answers
//! `g.V("1")`), multi-/meta-properties are folded into arrays and `_meta`, a label set is one
//! `"A::B"` label for TinkerGraph, unlabeled elements get the default labels `vertex`/`edge`,
//! unsupported types (and the date types without a twin) are skipped, a zone id and digits below
//! a microsecond are dropped, a duration with both months/days and a time part is exported as
//! `graphersal:Interval` (no `java.time` type holds it), an edge to a missing vertex and a
//! duplicate vertex id are skipped instead of failing, vertex-property ids are not kept.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Write as _};
use std::fs::File;
use std::io::{BufRead, Read as _};

use compact_str::CompactString;
use indexmap::IndexMap;

use crate::error::IOError;
use crate::graph::shape::{MapBuilder, ShapeTable};
use crate::graph::{Graph, GraphStorage, TraversalGraph};
use crate::traversal::memory_budget::UnitBudget;
use crate::value::depth::{DEFAULT_MAX_VALUE_DEPTH, property_exceeds_depth};
use crate::value::physical::{
    PhysicalError, PhysicalValue, read_json, write_float, write_json, write_json_string,
};
use crate::value::temporal::{DateTime, Duration, Temporal, TemporalKind, tags};
use crate::value::uuid::{format_canonical, parse_canonical};
use crate::value::{ElementProperty, N, Number, PropertyMap};

/// The reserved vertex property that holds imported meta-properties.
pub const META_KEY: &str = "_meta";

/// The label TinkerPop gives a vertex without one (`Vertex.DEFAULT_LABEL`); export writes it for
/// an unlabeled vertex.
pub const DEFAULT_VERTEX_LABEL: &str = "vertex";

/// The label TinkerPop gives an edge without one (`Edge.DEFAULT_LABEL`); export writes it for an
/// unlabeled edge.
pub const DEFAULT_EDGE_LABEL: &str = "edge";

/// The default of [`GraphSONImport::max_text_bytes`]: 512 MiB natively, 64 MiB on wasm32.
pub const DEFAULT_MAX_TEXT_BYTES: usize = if cfg!(target_arch = "wasm32") {
    64 << 20
} else {
    512 << 20
};

/// At most this many sample locations are kept per [`ImportNote`].
const MAX_SAMPLES: usize = 5;

/// The JSON nesting a line may have: a property value may nest [`DEFAULT_MAX_VALUE_DEPTH`]
/// logical levels, each typed container costs two JSON levels, plus the line's own structure.
const MAX_JSON_DEPTH: usize = DEFAULT_MAX_VALUE_DEPTH * 2 + 8;

/// Local names (after the `g:`/`gx:` prefix) of the date and time types Graphersal cannot hold:
/// always skipped (the others are read by [`decode_temporal`]).
const SKIPPED_DATE_TIME_TYPES: [&str; 7] = [
    "Calendar",
    "TimeZone",
    "OffsetTime",
    "Year",
    "YearMonth",
    "MonthDay",
    "ZoneOffset",
];

// ---------------------------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------------------------

/// What one [`ImportNote`] is about. The `Skipped*` kinds lost data; the others changed only the
/// representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImportNoteKind {
    /// A date or time value of a type Graphersal cannot hold (`gx:Year`, `gx:YearMonth`,
    /// `gx:MonthDay`, `gx:OffsetTime`, `gx:ZoneOffset`, ...), never converted to a string or a
    /// number.
    SkippedDateTime {
        /// The value's `@type`.
        type_tag: String,
    },
    /// A value of a type Graphersal cannot hold (`gx:BigDecimal`, `g:Class`, `g:Vertex`, a vendor
    /// type such as `janusgraph:Geoshape`, ...).
    SkippedUnsupportedType {
        /// The value's `@type`.
        type_tag: String,
    },
    /// A value whose payload does not fit its type (a `g:Int32` holding a string, an odd-length
    /// `g:Map`, a `g:Map` key that is not a string, an `@type` object with other fields, a value
    /// nested deeper than the value depth limit).
    SkippedMalformedValue,
    /// A vertex-property entry or an edge entry without the expected shape (not an object, no
    /// `value`, no `inV`/`outV`).
    SkippedMalformedEntry,
    /// A vertex whose id an earlier vertex of the file already has (the first one wins; the
    /// skipped vertex's edges are skipped with it).
    SkippedDuplicateVertex,
    /// An edge whose other endpoint is neither in the file nor in the target graph.
    SkippedDanglingEdge,
    /// An edge whose id an earlier, different edge of the file already has.
    SkippedDuplicateEdge,
    /// A `_meta` property of a vertex that also has meta-properties: `_meta` holds those.
    SkippedReservedMeta,
    /// A vertex property with several entries (list cardinality), imported as one array.
    MultiValuedToArray,
    /// Meta-properties, imported into the reserved `_meta` object property.
    MetaPropertiesToMeta,
    /// A `"A::B"` vertex label, split into the label set `[A, B]`.
    LabelSplit,
    /// A vertex without an id: the graph assigned one.
    VertexWithoutId,
    /// The `inE` copy of an edge disagrees with its `outE` copy (label or endpoints): the `outE`
    /// copy is kept.
    EdgeCopiesDisagree,
    /// A key the GraphSON graph format does not define (for example Cosmos DB's `type`), ignored.
    IgnoredKey {
        /// The key.
        key: String,
    },
    /// A `gx:ZonedDateTime` imported as a `datetime` with its offset at that time: the zone id
    /// (`Europe/Paris`) is not kept (a value never stores a zone).
    ZoneDropped {
        /// The value's `@type`.
        type_tag: String,
    },
    /// A date or time value with digits below a microsecond (Java writes nanoseconds): cut to
    /// microseconds, Graphersal's resolution.
    SubMicrosecondsCut {
        /// The value's `@type`.
        type_tag: String,
    },
}

impl ImportNoteKind {
    /// Whether this kind of note means that data was not imported.
    pub fn is_skip(&self) -> bool {
        match self {
            ImportNoteKind::SkippedDateTime { .. }
            | ImportNoteKind::SkippedUnsupportedType { .. }
            | ImportNoteKind::SkippedMalformedValue
            | ImportNoteKind::SkippedMalformedEntry
            | ImportNoteKind::SkippedDuplicateVertex
            | ImportNoteKind::SkippedDanglingEdge
            | ImportNoteKind::SkippedDuplicateEdge
            | ImportNoteKind::SkippedReservedMeta => true,
            ImportNoteKind::MultiValuedToArray
            | ImportNoteKind::MetaPropertiesToMeta
            | ImportNoteKind::LabelSplit
            | ImportNoteKind::VertexWithoutId
            | ImportNoteKind::EdgeCopiesDisagree
            | ImportNoteKind::IgnoredKey { .. }
            | ImportNoteKind::ZoneDropped { .. }
            | ImportNoteKind::SubMicrosecondsCut { .. } => false,
        }
    }
}

impl fmt::Display for ImportNoteKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportNoteKind::SkippedDateTime { type_tag } => {
                write!(
                    f,
                    "skipped date/time values ({type_tag}, a type Graphersal cannot hold)"
                )
            }
            ImportNoteKind::SkippedUnsupportedType { type_tag } => {
                write!(f, "skipped values of the unsupported type {type_tag}")
            }
            ImportNoteKind::SkippedMalformedValue => f.write_str("skipped malformed values"),
            ImportNoteKind::SkippedMalformedEntry => {
                f.write_str("skipped malformed property or edge entries")
            }
            ImportNoteKind::SkippedDuplicateVertex => {
                f.write_str("skipped vertices with an id already used in the file")
            }
            ImportNoteKind::SkippedDanglingEdge => {
                f.write_str("skipped edges to a vertex that does not exist")
            }
            ImportNoteKind::SkippedDuplicateEdge => {
                f.write_str("skipped edges with an id already used in the file")
            }
            ImportNoteKind::SkippedReservedMeta => write!(
                f,
                "skipped '{META_KEY}' properties of vertices with meta-properties \
                 ('{META_KEY}' holds those)"
            ),
            ImportNoteKind::MultiValuedToArray => {
                f.write_str("multi-valued vertex properties imported as arrays")
            }
            ImportNoteKind::MetaPropertiesToMeta => {
                write!(f, "meta-properties imported into '{META_KEY}'")
            }
            ImportNoteKind::LabelSplit => f.write_str("'::' labels split into label sets"),
            ImportNoteKind::VertexWithoutId => f.write_str("vertices without an id were given one"),
            ImportNoteKind::EdgeCopiesDisagree => {
                f.write_str("edges whose inE copy disagrees with the outE copy (outE kept)")
            }
            ImportNoteKind::IgnoredKey { key } => write!(f, "ignored the unknown key '{key}'"),
            ImportNoteKind::ZoneDropped { type_tag } => write!(
                f,
                "{type_tag} values imported as datetime with their offset (the zone id is not kept)"
            ),
            ImportNoteKind::SubMicrosecondsCut { type_tag } => write!(
                f,
                "{type_tag} values cut to microseconds (digits below a microsecond dropped)"
            ),
        }
    }
}

/// One group of an [`ImportReport`]: what happened, how often, and where (the first few places).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportNote {
    /// What happened.
    pub kind: ImportNoteKind,
    /// How often it happened.
    pub count: usize,
    /// The first places it happened (`line 3: vertex "1", property "born"`), at most five.
    pub samples: Vec<String>,
}

/// What a GraphSON import did: the counts and the notes about skipped or converted data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportReport {
    /// Vertices imported.
    pub vertices: usize,
    /// Edges imported.
    pub edges: usize,
    /// The notes, in the order they first occurred.
    pub notes: Vec<ImportNote>,
}

impl ImportReport {
    /// How many values, entries or elements were skipped in total.
    pub fn skipped(&self) -> usize {
        self.notes
            .iter()
            .filter(|note| note.kind.is_skip())
            .map(|note| note.count)
            .sum()
    }

    /// Whether anything was skipped.
    pub fn has_skips(&self) -> bool {
        self.skipped() > 0
    }
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "imported {} vertices and {} edges",
            self.vertices, self.edges
        )?;
        for note in &self.notes {
            write!(f, "\n- {}: {}", note.kind, note.count)?;
            if !note.samples.is_empty() {
                write!(f, " (e.g. {})", note.samples.join("; "))?;
            }
        }
        Ok(())
    }
}

/// Collects the notes while importing.
#[derive(Default)]
struct ReportBuilder {
    notes: IndexMap<ImportNoteKind, (usize, Vec<String>)>,
}

impl ReportBuilder {
    fn note(&mut self, kind: ImportNoteKind, sample: impl FnOnce() -> String) {
        let (count, samples) = self.notes.entry(kind).or_default();
        *count += 1;
        if samples.len() < MAX_SAMPLES {
            samples.push(sample());
        }
    }

    fn finish(self, vertices: usize, edges: usize) -> ImportReport {
        ImportReport {
            vertices,
            edges,
            notes: self
                .notes
                .into_iter()
                .map(|(kind, (count, samples))| ImportNote {
                    kind,
                    count,
                    samples,
                })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Value decoding
// ---------------------------------------------------------------------------------------------

/// Why a value was skipped.
struct ValueSkip {
    kind: ImportNoteKind,
    reason: Option<&'static str>,
}

impl ValueSkip {
    fn malformed(reason: &'static str) -> Self {
        ValueSkip {
            kind: ImportNoteKind::SkippedMalformedValue,
            reason: Some(reason),
        }
    }
}

/// Splits a typed value `{"@type": T, "@value": V}` (exactly these two keys, in either order) into
/// `(T, V)`. `Ok(Err(fields))` gives back a plain object; `Err` is an `@type` object with another
/// shape.
#[allow(clippy::type_complexity)]
fn split_typed(
    fields: Vec<(Cow<'static, str>, PhysicalValue<'static>)>,
) -> Result<
    Result<(String, PhysicalValue<'static>), Vec<(Cow<'static, str>, PhysicalValue<'static>)>>,
    ValueSkip,
> {
    let has_type = fields.iter().any(|(key, _)| key == "@type");
    if !has_type {
        return Ok(Err(fields));
    }
    if fields.len() != 2 {
        return Err(ValueSkip::malformed(
            "an @type object must have exactly the keys @type and @value",
        ));
    }
    let mut tag = None;
    let mut payload = None;
    for (key, value) in fields {
        match (key.as_ref(), value) {
            ("@type", PhysicalValue::String(text)) => tag = Some(text.into_owned()),
            ("@value", value) => payload = Some(value),
            _ => {}
        }
    }
    match (tag, payload) {
        (Some(tag), Some(payload)) => Ok(Ok((tag, payload))),
        _ => Err(ValueSkip::malformed(
            "an @type object must have exactly the keys @type and @value",
        )),
    }
}

/// The local name of a type tag (`g:Int32` -> `Int32`).
fn local_name(tag: &str) -> &str {
    tag.rsplit_once(':').map_or(tag, |(_, name)| name)
}

/// Decodes one GraphSON value (typed or plain) into a storable logical value.
fn decode_value(
    value: PhysicalValue<'static>,
    notes: &mut Vec<ImportNoteKind>,
) -> Result<ElementProperty, ValueSkip> {
    match value {
        PhysicalValue::Null => Ok(ElementProperty::Null),
        PhysicalValue::Bool(b) => Ok(ElementProperty::Boolean(b)),
        PhysicalValue::Int(i) => Ok(ElementProperty::Number(Number::from_i64(i))),
        PhysicalValue::Float(f) => Ok(ElementProperty::Number(Number::from_f64(f))),
        PhysicalValue::String(s) => Ok(ElementProperty::String(CompactString::from(s))),
        PhysicalValue::Array(items) => decode_list(items, notes),
        PhysicalValue::Object(fields) => match split_typed(fields)? {
            Ok((tag, payload)) => decode_typed(&tag, payload, notes),
            Err(fields) => {
                let mut map = PropertyMap::with_capacity(fields.len());
                for (key, field) in fields {
                    map.insert(CompactString::from(key), decode_value(field, notes)?);
                }
                Ok(ElementProperty::Object(Box::new(map)))
            }
        },
    }
}

fn decode_list(
    items: Vec<PhysicalValue<'static>>,
    notes: &mut Vec<ImportNoteKind>,
) -> Result<ElementProperty, ValueSkip> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(decode_value(item, notes)?);
    }
    Ok(ElementProperty::Array(out))
}

fn decode_typed(
    tag: &str,
    payload: PhysicalValue<'static>,
    notes: &mut Vec<ImportNoteKind>,
) -> Result<ElementProperty, ValueSkip> {
    let core = tag.starts_with("g:") || tag.starts_with("gx:");
    let name = local_name(tag);
    if core && SKIPPED_DATE_TIME_TYPES.contains(&name) {
        return Err(ValueSkip {
            kind: ImportNoteKind::SkippedDateTime {
                type_tag: tag.to_string(),
            },
            reason: None,
        });
    }
    if core && let Some(decoded) = decode_temporal(tag, name, &payload, notes) {
        return decoded.map(ElementProperty::Temporal);
    }
    let unsupported = || ValueSkip {
        kind: ImportNoteKind::SkippedUnsupportedType {
            type_tag: tag.to_string(),
        },
        reason: None,
    };
    // Our own tag of a duration no `java.time` type holds (what an export writes).
    if tag == tags::INTERVAL {
        return match &payload {
            PhysicalValue::String(text) => tags::read_tagged(tag, text)
                .and_then(Result::ok)
                .map(ElementProperty::Temporal)
                .ok_or_else(|| {
                    ValueSkip::malformed(
                        "graphersal:Interval needs a duration text with both months or days and \
                         a time part",
                    )
                }),
            _ => Err(ValueSkip::malformed(
                "graphersal:Interval needs a duration text",
            )),
        };
    }
    // Our own exact vector tag (what a serialized `ChangeSet` writes; an export writes a
    // `g:List` of `g:Float`, which stays a list unless the target schema declares a vector).
    if tag == crate::value::vector::tagged::VECTOR_TAG {
        return crate::value::vector::tagged::from_payload(&payload)
            .map(ElementProperty::Vector)
            .map_err(|_| {
                ValueSkip::malformed(
                    "graphersal:Vector needs {\"element\", \"data\"} (or the i8 or bit fields)",
                )
            });
    }
    if !core {
        return Err(unsupported());
    }
    match (name, payload) {
        ("Int32" | "Int64" | "Int16" | "Byte" | "BigInteger", PhysicalValue::Int(i)) => {
            Ok(ElementProperty::Number(Number::from_i64(i)))
        }
        ("Int32" | "Int64" | "Int16" | "Byte" | "BigInteger", _) => Err(ValueSkip::malformed(
            "an integer type needs an integer payload",
        )),
        ("Double" | "Float", PhysicalValue::Float(f)) => {
            Ok(ElementProperty::Number(Number::from_f64(f)))
        }
        ("Double" | "Float", PhysicalValue::Int(i)) => {
            Ok(ElementProperty::Number(Number::from_f64(i as f64)))
        }
        ("Double" | "Float", PhysicalValue::String(text)) => match text.as_ref() {
            "NaN" => Ok(ElementProperty::Number(Number::from_f64(f64::NAN))),
            "Infinity" => Ok(ElementProperty::Number(Number::from_f64(f64::INFINITY))),
            "-Infinity" => Ok(ElementProperty::Number(Number::from_f64(f64::NEG_INFINITY))),
            _ => Err(ValueSkip::malformed(
                "a floating-point type needs a number payload",
            )),
        },
        ("Double" | "Float", _) => Err(ValueSkip::malformed(
            "a floating-point type needs a number payload",
        )),
        ("UUID", PhysicalValue::String(text)) => parse_canonical(&text.to_ascii_lowercase())
            .map(ElementProperty::Uuid)
            .ok_or_else(|| ValueSkip::malformed("g:UUID needs a canonical UUID string")),
        ("UUID", _) => Err(ValueSkip::malformed("g:UUID needs a canonical UUID string")),
        ("Char", PhysicalValue::String(text)) => {
            Ok(ElementProperty::String(CompactString::from(text)))
        }
        ("Char", _) => Err(ValueSkip::malformed("gx:Char needs a string payload")),
        ("List" | "Set", PhysicalValue::Array(items)) => decode_list(items, notes),
        ("List" | "Set", _) => Err(ValueSkip::malformed("g:List/g:Set need an array payload")),
        ("Map", PhysicalValue::Array(flat)) => {
            if flat.len() % 2 != 0 {
                return Err(ValueSkip::malformed("g:Map needs an even number of items"));
            }
            let mut map = PropertyMap::with_capacity(flat.len() / 2);
            let mut items = flat.into_iter();
            while let (Some(key), Some(value)) = (items.next(), items.next()) {
                let ElementProperty::String(key) = decode_value(key, notes)? else {
                    return Err(ValueSkip::malformed("a g:Map key must be a string"));
                };
                map.insert(key, decode_value(value, notes)?);
            }
            Ok(ElementProperty::Object(Box::new(map)))
        }
        ("Map", _) => Err(ValueSkip::malformed("g:Map needs an array payload")),
        _ => Err(unsupported()),
    }
}

/// Decodes a `java.time` value (the `g:`/`gx:` date and time types of GraphSON) into a temporal
/// value; `None` when `name` is not one of them.
///
/// - `g:Date`, `g:Timestamp`: epoch milliseconds -> `datetime` in UTC;
/// - `gx:Instant`, `gx:OffsetDateTime` (also `g:DateTime`) -> `datetime` with its offset;
///   `gx:ZonedDateTime` -> `datetime` with the offset it states, the zone id is dropped
///   ([`ImportNoteKind::ZoneDropped`]);
/// - `gx:LocalDate`, `gx:LocalDateTime`, `gx:LocalTime` -> `date`, `localdatetime`, `time`;
/// - `gx:Duration` (a fixed length; a day is 24 hours) and `gx:Period` (years, months, days) ->
///   `duration`.
///
/// Digits below a microsecond are cut ([`ImportNoteKind::SubMicrosecondsCut`]); a value outside
/// the supported range or text that is not of the type is a malformed value.
fn decode_temporal(
    tag: &str,
    name: &str,
    payload: &PhysicalValue<'_>,
    notes: &mut Vec<ImportNoteKind>,
) -> Option<Result<Temporal, ValueSkip>> {
    const NOT_OF_TYPE: &str =
        "the text is not a value of the date/time type, or lies outside the years -9999 to 9999";
    let kind = match name {
        "Date" | "Timestamp" => {
            return Some(match payload {
                PhysicalValue::Int(millis) => DateTime::from_epoch_millis(*millis)
                    .map(Temporal::DateTime)
                    .map_err(|_| {
                        ValueSkip::malformed("the date lies outside the years -9999 to 9999")
                    }),
                _ => Err(ValueSkip::malformed(
                    "g:Date and g:Timestamp need the epoch milliseconds",
                )),
            });
        }
        "Instant" | "OffsetDateTime" | "ZonedDateTime" | "DateTime" => TemporalKind::DateTime,
        "LocalDate" => TemporalKind::Date,
        "LocalDateTime" => TemporalKind::LocalDateTime,
        "LocalTime" => TemporalKind::Time,
        "Duration" | "Period" => TemporalKind::Duration,
        _ => return None,
    };
    let PhysicalValue::String(text) = payload else {
        return Some(Err(ValueSkip::malformed(
            "a date or time type needs its text",
        )));
    };
    let text = text.as_ref();
    let (text, zone) = match (name, text.split_once('[')) {
        ("ZonedDateTime", Some((local, _))) if has_offset(local) => (local, true),
        _ => (text, false),
    };
    let Ok(value) = Temporal::parse_strict(kind, text) else {
        return Some(Err(ValueSkip::malformed(NOT_OF_TYPE)));
    };
    let value = match (name, value) {
        ("Duration", Temporal::Duration(duration)) => {
            if duration.months() != 0 {
                return Some(Err(ValueSkip::malformed(
                    "a gx:Duration has no years or months (that is a gx:Period)",
                )));
            }
            match (duration.days() as i64)
                .checked_mul(86_400_000_000)
                .and_then(|micros| micros.checked_add(duration.micros()))
            {
                Some(micros) => Temporal::Duration(Duration::from_micros(micros)),
                None => return Some(Err(ValueSkip::malformed(NOT_OF_TYPE))),
            }
        }
        ("Period", Temporal::Duration(duration)) if duration.micros() != 0 => {
            return Some(Err(ValueSkip::malformed(
                "a gx:Period has no time part (that is a gx:Duration)",
            )));
        }
        (_, value) => value,
    };
    if zone {
        notes.push(ImportNoteKind::ZoneDropped {
            type_tag: tag.to_string(),
        });
    }
    if cuts_below_micros(text) {
        notes.push(ImportNoteKind::SubMicrosecondsCut {
            type_tag: tag.to_string(),
        });
    }
    Some(Ok(value))
}

/// Whether a date-time text ends in a UTC offset (`Z`, `+02:00`, `-05:00`).
fn has_offset(text: &str) -> bool {
    match text.split_once(['T', 't']) {
        Some((_, time)) => time.ends_with(['Z', 'z']) || time.contains(['+', '-']),
        None => false,
    }
}

/// Whether a text has a fraction of a second with a non-zero digit below a microsecond.
fn cuts_below_micros(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if matches!(bytes[at], b'.' | b',') {
            let digits = bytes[at + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if bytes[at + 1..at + 1 + digits]
                .iter()
                .skip(6)
                .any(|&b| b != b'0')
            {
                return true;
            }
            at += digits;
        }
        at += 1;
    }
    false
}

/// Decodes a property value and checks its depth.
fn decode_property(
    value: PhysicalValue<'static>,
    notes: &mut Vec<ImportNoteKind>,
) -> Result<ElementProperty, ValueSkip> {
    let decoded = decode_value(value, notes)?;
    if property_exceeds_depth(&decoded, DEFAULT_MAX_VALUE_DEPTH) {
        return Err(ValueSkip::malformed("the value is nested too deeply"));
    }
    Ok(decoded)
}

/// The compact JSON text of a physical value (for an id of an unknown type).
fn json_text(value: &PhysicalValue<'_>) -> CompactString {
    let mut out = String::new();
    match write_json(value, &mut out) {
        Ok(()) => CompactString::from(out),
        Err(_) => CompactString::default(),
    }
}

/// The text of a number written as an id.
fn float_text(value: f64) -> CompactString {
    let mut out = String::new();
    match write_float(value, &mut out) {
        Ok(()) => CompactString::from(out),
        Err(_) => CompactString::from(value.to_string()),
    }
}

/// Decodes an element id: every id becomes a string (`g:Int32 1` -> `"1"`, a `g:UUID` -> its
/// canonical text, a vendor id `{"relationId": "x"}` -> `"x"`, anything else -> its JSON text).
/// `None` for a null id.
fn decode_id(value: PhysicalValue<'static>) -> Option<CompactString> {
    match value {
        PhysicalValue::Null => None,
        PhysicalValue::Bool(b) => Some(CompactString::from(if b { "true" } else { "false" })),
        PhysicalValue::Int(i) => Some(CompactString::from(i.to_string())),
        PhysicalValue::Float(f) => Some(float_text(f)),
        PhysicalValue::String(s) => Some(CompactString::from(s)),
        PhysicalValue::Array(_) => Some(json_text(&value)),
        PhysicalValue::Object(fields) => {
            let fields = match split_typed(fields) {
                Ok(Ok((tag, payload))) => return Some(decode_typed_id(&tag, payload)),
                Ok(Err(fields)) => fields,
                Err(_) => return None,
            };
            Some(json_text(&PhysicalValue::Object(fields)))
        }
    }
}

fn decode_typed_id(tag: &str, payload: PhysicalValue<'static>) -> CompactString {
    match payload {
        PhysicalValue::String(text) if local_name(tag) == "UUID" => {
            let lower = text.to_ascii_lowercase();
            match parse_canonical(&lower) {
                Some(uuid) => format_canonical(uuid),
                None => CompactString::from(text),
            }
        }
        PhysicalValue::String(text) => CompactString::from(text),
        PhysicalValue::Int(i) => CompactString::from(i.to_string()),
        PhysicalValue::Float(f) => float_text(f),
        PhysicalValue::Object(ref fields) => {
            // JanusGraph's `janusgraph:RelationIdentifier` carries its id as `relationId`.
            match fields.iter().rev().find(|(key, _)| key == "relationId") {
                Some((_, PhysicalValue::String(id))) => CompactString::from(id.as_ref()),
                _ => json_text(&payload),
            }
        }
        other => json_text(&other),
    }
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

/// GraphSON importer: reads GraphSON into a [`GraphStorage`] and returns an [`ImportReport`].
///
/// Build it with [`GraphSONImport::new`]. GraphSON carries no schema: a typed value keeps its own
/// type, and the target graph's stored schema coerces and validates on write as for any other
/// write (set the schema first, then import).
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct GraphSONImport {
    /// The largest line (line form) or document (wrapped form) read, in bytes; a larger one fails
    /// with [`IOError::InputTooLarge`] before it is held in memory.
    pub max_text_bytes: usize,
}

impl Default for GraphSONImport {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a record came from, for messages.
#[derive(Clone, Copy)]
enum Framing {
    Lines,
    Wrapped,
}

/// A vertex-property entry, decoded.
struct Entry {
    value: ElementProperty,
    meta: PropertyMap,
}

/// An edge read from an `outE` or `inE` list, applied once every vertex is known.
struct PendingEdge {
    id: Option<CompactString>,
    label: CompactString,
    from: CompactString,
    to: CompactString,
    properties: PropertyMap,
    record: usize,
}

/// The operation `GraphError::ResourceLimitExceeded` names for a GraphSON import.
pub(crate) const GRAPHSON_IMPORT: &str = "GraphSON import";

/// The state of one import.
struct Importer<'g, S: GraphStorage> {
    g: &'g mut S,
    framing: Framing,
    report: ReportBuilder,
    vertices: HashMap<CompactString, S::VertexId>,
    out_edges: Vec<PendingEdge>,
    in_edges: Vec<PendingEdge>,
    /// Vertices whose record had an `outE` key: their `inE`-only edges without an id are copies.
    has_out_e: HashSet<CompactString>,
    vertex_count: usize,
    edge_count: usize,
    /// The memory budget of a live import (`None`: a load, or no budget configured).
    budget: &'g mut Option<UnitBudget>,
    /// The vertex property map being decoded (built `Shaped`, see [`MapBuilder`]).
    vertex_map: MapBuilder,
    /// The importer's own shapes of vertex maps (the graph adopts them).
    vertex_shapes: ShapeTable,
    /// The edge property map being decoded.
    edge_map: MapBuilder,
    /// The importer's own shapes of edge maps.
    edge_shapes: ShapeTable,
}

impl GraphSONImport {
    /// A new importer with the default limits.
    pub fn new() -> Self {
        Self {
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
        }
    }

    /// Sets [`Self::max_text_bytes`].
    pub fn with_max_text_bytes(mut self, max_text_bytes: usize) -> Self {
        self.max_text_bytes = max_text_bytes;
        self
    }

    /// Imports the GraphSON file at `path` into the live graph `g` (one unit, see
    /// [`Self::import_reader`]).
    pub fn import_file<S: GraphStorage>(
        &self,
        g: &mut S,
        path: &str,
    ) -> Result<ImportReport, IOError> {
        let f = File::open(path).map_err(|err| IOError::FileRead {
            path: path.to_string(),
            source: err,
        })?;
        self.import_reader(g, std::io::BufReader::with_capacity(128 * 1024, f))
    }

    /// Imports GraphSON read from `reader` into the live graph `g`.
    ///
    /// The import is ONE unit of work: on an atomic storage a failure leaves nothing behind, and
    /// its commit is captured and runs the commit hooks like a traversal (a veto is
    /// [`IOError::Graph`] holding a `GraphError::CommitRejected`). Skipped data is not a failure:
    /// it is listed in the returned report.
    ///
    /// Under a memory budget (a `memory.limit` default of the graph's
    /// [`ExecutionPolicy`](crate::graph::ExecutionPolicy)) the import checks it every 4096
    /// elements and at its end: an import that grows the memory in use over it is rolled back
    /// and fails with [`IOError::Graph`] holding a `GraphError::ResourceLimitExceeded`
    /// (operation `"GraphSON import"`). Without a budget nothing is checked.
    pub fn import_reader<S: GraphStorage, R: BufRead>(
        &self,
        g: &mut S,
        reader: R,
    ) -> Result<ImportReport, IOError> {
        self.import_reader_with(g, reader, None)
    }

    /// [`Self::import_reader`] whose memory budget also honours a host's preset options
    /// (`presets`, such as a script session's: they win over the policy's default unless the
    /// policy locks the key).
    pub(crate) fn import_reader_with<S: GraphStorage, R: BufRead>(
        &self,
        g: &mut S,
        reader: R,
        presets: Option<&HashMap<CompactString, ElementProperty>>,
    ) -> Result<ImportReport, IOError> {
        let mut budget = UnitBudget::start(&*g, presets, GRAPHSON_IMPORT);
        let mark = g.begin_unit();
        let imported = crate::graph::storage::close_on_unwind(
            g,
            |g| {
                let report = self.import_outside_units(g, reader, &mut budget)?;
                crate::traversal::memory_budget::check(&budget, &*g).map_err(IOError::Graph)?;
                Ok(report)
            },
            |g| {
                g.rollback_unit(
                    mark,
                    &crate::graph::RollbackReason::failed(&crate::graph::storage::UNWOUND),
                )
            },
        );
        match imported {
            Ok(report) => {
                g.commit_unit(mark).map_err(IOError::Graph)?;
                Ok(report)
            }
            Err(err) => {
                g.rollback_unit(mark, &crate::graph::RollbackReason::failed(&err));
                Err(err)
            }
        }
    }

    /// Loads a new graph from GraphSON read from `reader`, outside units (nobody sees the graph
    /// yet), and returns it with the report. [`GraphSource::from_graphson_reader`](crate::GraphSource::from_graphson_reader)
    /// is this without the report.
    pub fn load<R: BufRead>(&self, reader: R) -> Result<(Graph, ImportReport), IOError> {
        let mut graph = TraversalGraph::new();
        let report = graph
            .load_outside_units(|graph| self.import_outside_units(graph, reader, &mut None))?;
        Ok((Graph::new(graph), report))
    }

    /// [`Self::load`] from the GraphSON file at `path`.
    pub fn load_file(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(Graph, ImportReport), IOError> {
        let path = path.as_ref();
        let f = File::open(path).map_err(|err| IOError::FileRead {
            path: path.to_string_lossy().into_owned(),
            source: err,
        })?;
        self.load(std::io::BufReader::with_capacity(128 * 1024, f))
    }

    /// The import itself, without opening a unit. `budget` is the memory budget of a live
    /// import (checked every few thousand elements); `None` for a load.
    pub(crate) fn import_outside_units<S: GraphStorage, R: BufRead>(
        &self,
        g: &mut S,
        mut reader: R,
        budget: &mut Option<UnitBudget>,
    ) -> Result<ImportReport, IOError> {
        let mut importer = Importer {
            g,
            budget,
            framing: Framing::Lines,
            report: ReportBuilder::default(),
            vertices: HashMap::new(),
            out_edges: Vec::new(),
            in_edges: Vec::new(),
            has_out_e: HashSet::new(),
            vertex_count: 0,
            edge_count: 0,
            vertex_map: MapBuilder::default(),
            vertex_shapes: ShapeTable::new(),
            edge_map: MapBuilder::default(),
            edge_shapes: ShapeTable::new(),
        };
        let mut lines = Lines {
            limit: self.max_text_bytes,
            line: 0,
            buf: Vec::new(),
        };
        let mut first = true;
        while let Some(text) = lines.next(&mut reader)? {
            let line = lines.line;
            let text = if first {
                text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()
            } else {
                text
            };
            if text.trim().is_empty() {
                continue;
            }
            if !first {
                let value = parse_line(&text, line)?;
                importer.vertex(value, line)?;
                continue;
            }
            first = false;
            match read_json(&text, MAX_JSON_DEPTH) {
                Ok(value) if is_wrapped(&value) => {
                    importer.framing = Framing::Wrapped;
                    importer.wrapped(value)?;
                }
                Ok(value) => importer.vertex(value, line)?,
                Err(line_error) => {
                    // Not one JSON value per line. Only a first line that opens a document (`{`
                    // alone, or `{"vertices": ...` as TinkerPop's wrapped writer starts) can be a
                    // pretty-printed wrapped document (or one pretty-printed vertex): then read
                    // the rest and parse the whole text. Anything else is a broken line.
                    if !opens_document(&text) {
                        return Err(IOError::GraphSONParse {
                            line,
                            message: line_error.to_string(),
                        });
                    }
                    let mut whole = text;
                    whole.push('\n');
                    let budget = self.max_text_bytes.saturating_sub(whole.len());
                    let mut rest = Vec::new();
                    (&mut reader)
                        .take(budget as u64 + 1)
                        .read_to_end(&mut rest)
                        .map_err(|source| IOError::FileRead {
                            path: "<GraphSON input>".to_string(),
                            source,
                        })?;
                    if rest.len() > budget {
                        return Err(IOError::InputTooLarge {
                            line,
                            limit: self.max_text_bytes,
                        });
                    }
                    let rest = String::from_utf8(rest).map_err(|_| IOError::GraphSONParse {
                        line,
                        message: "the input is not valid UTF-8".to_string(),
                    })?;
                    whole.push_str(&rest);
                    match read_json(&whole, MAX_JSON_DEPTH) {
                        Ok(value) if is_wrapped(&value) => {
                            importer.framing = Framing::Wrapped;
                            importer.wrapped(value)?;
                        }
                        Ok(value) => importer.vertex(value, line)?,
                        Err(_) => {
                            return Err(IOError::GraphSONParse {
                                line,
                                message: line_error.to_string(),
                            });
                        }
                    }
                    break;
                }
            }
        }
        importer.edges()?;
        importer.g.shrink_to_fit();
        let (vertices, edges) = (importer.vertex_count, importer.edge_count);
        Ok(importer.report.finish(vertices, edges))
    }
}

/// Parses one line of the line form.
fn parse_line(text: &str, line: usize) -> Result<PhysicalValue<'static>, IOError> {
    read_json(text, MAX_JSON_DEPTH).map_err(|err| IOError::GraphSONParse {
        line,
        message: err.to_string(),
    })
}

/// Whether a first line that is no complete JSON value starts a multi-line document: `{` alone,
/// or `{` followed by the `"vertices"` key.
fn opens_document(first_line: &str) -> bool {
    let trimmed = first_line.trim();
    trimmed == "{"
        || trimmed
            .strip_prefix('{')
            .is_some_and(|rest| rest.trim_start().starts_with("\"vertices\""))
}

/// Whether a top-level value is the wrapped form (`{"vertices": [...]}`, no vertex keys).
fn is_wrapped(value: &PhysicalValue<'_>) -> bool {
    match value {
        PhysicalValue::Object(fields) => {
            fields.iter().any(|(key, _)| key == "vertices")
                && !fields
                    .iter()
                    .any(|(key, _)| matches!(key.as_ref(), "id" | "label" | "properties"))
        }
        _ => false,
    }
}

/// Reads the input line by line, never holding more than `limit` bytes of one line.
struct Lines {
    limit: usize,
    line: usize,
    buf: Vec<u8>,
}

impl Lines {
    fn next<R: BufRead>(&mut self, reader: &mut R) -> Result<Option<String>, IOError> {
        self.buf.clear();
        self.line += 1;
        let read = reader
            .take(self.limit as u64 + 2)
            .read_until(b'\n', &mut self.buf)
            .map_err(|source| IOError::FileRead {
                path: "<GraphSON input>".to_string(),
                source,
            })?;
        if read == 0 {
            return Ok(None);
        }
        if self.buf.last() == Some(&b'\n') {
            self.buf.pop();
            if self.buf.last() == Some(&b'\r') {
                self.buf.pop();
            }
        }
        if self.buf.len() > self.limit {
            return Err(IOError::InputTooLarge {
                line: self.line,
                limit: self.limit,
            });
        }
        let text = String::from_utf8(std::mem::take(&mut self.buf)).map_err(|_| {
            IOError::GraphSONParse {
                line: self.line,
                message: "the line is not valid UTF-8".to_string(),
            }
        })?;
        Ok(Some(text))
    }
}

impl<S: GraphStorage> Importer<'_, S> {
    /// The location of a record, for messages and samples.
    fn location(&self, record: usize) -> String {
        match self.framing {
            Framing::Lines => format!("line {record}"),
            Framing::Wrapped => format!("vertices[{}]", record.saturating_sub(1)),
        }
    }

    /// Imports every vertex of the wrapped form.
    fn wrapped(&mut self, value: PhysicalValue<'static>) -> Result<(), IOError> {
        let PhysicalValue::Object(fields) = value else {
            return Ok(());
        };
        let mut vertices = None;
        for (key, field) in fields {
            match key.as_ref() {
                "vertices" => vertices = Some(field),
                "mode" => {
                    return Err(IOError::UnsupportedGraphSON {
                        line: 1,
                        reason: "this is the TinkerPop 2 (Blueprints) GraphSON format".to_string(),
                    });
                }
                other => self.report.note(
                    ImportNoteKind::IgnoredKey {
                        key: other.to_string(),
                    },
                    || "top level".to_string(),
                ),
            }
        }
        let Some(PhysicalValue::Array(vertices)) = vertices else {
            return Err(IOError::GraphSONParse {
                line: 1,
                message: "\"vertices\" must be an array of vertex objects".to_string(),
            });
        };
        for (index, vertex) in vertices.into_iter().enumerate() {
            self.vertex(vertex, index + 1)?;
        }
        Ok(())
    }

    /// Imports one vertex record and buffers its edges.
    fn vertex(&mut self, value: PhysicalValue<'static>, record: usize) -> Result<(), IOError> {
        let PhysicalValue::Object(fields) = value else {
            return Err(IOError::GraphSONParse {
                line: record,
                message: format!("{}: a vertex must be a JSON object", self.location(record)),
            });
        };
        let mut id = None;
        let mut label = None;
        let mut properties = None;
        let mut out_e = None;
        let mut in_e = None;
        for (key, field) in fields {
            match key.as_ref() {
                "id" => id = Some(field),
                "label" => label = Some(field),
                "properties" => properties = Some(field),
                "outE" => out_e = Some(field),
                "inE" => in_e = Some(field),
                "@class" => {
                    return Err(IOError::UnsupportedGraphSON {
                        line: record,
                        reason: "this is typed GraphSON 1.0 (@class)".to_string(),
                    });
                }
                "_id" | "_type" => {
                    return Err(IOError::UnsupportedGraphSON {
                        line: record,
                        reason: "this is the TinkerPop 2 (Blueprints) GraphSON format".to_string(),
                    });
                }
                other => {
                    let at = self.location(record);
                    self.report.note(
                        ImportNoteKind::IgnoredKey {
                            key: other.to_string(),
                        },
                        || at,
                    );
                }
            }
        }
        let id = id.and_then(decode_id);
        let at = self.location(record);
        let shown_id = id.as_deref().unwrap_or("").to_string();

        if let Some(existing) = &id
            && self.vertices.contains_key(existing)
        {
            self.report
                .note(ImportNoteKind::SkippedDuplicateVertex, || {
                    format!("{at}: vertex \"{existing}\"")
                });
            return Ok(());
        }

        let labels = match label {
            Some(PhysicalValue::String(text)) => {
                if text.contains("::") {
                    self.report.note(ImportNoteKind::LabelSplit, || {
                        format!("{at}: vertex \"{shown_id}\", label \"{text}\"")
                    });
                }
                text.split("::")
                    .filter(|part| !part.is_empty())
                    .map(CompactString::new)
                    .collect()
            }
            _ => Vec::new(),
        };

        let data = self.vertex_properties(properties, &at, &shown_id);

        let had_id = id.is_some();
        let handle = self
            .g
            .insert_vertex(id, labels, data)
            .map_err(IOError::Graph)?;
        let vertex_id = match self.g.vertex_id(handle) {
            Some(text) => CompactString::from(text),
            None => CompactString::default(),
        };
        if !had_id {
            self.report.note(ImportNoteKind::VertexWithoutId, || {
                format!("{at}: given id \"{vertex_id}\"")
            });
        }
        self.vertices.insert(vertex_id.clone(), handle);
        self.vertex_count += 1;
        crate::traversal::memory_budget::tick(self.budget, &*self.g).map_err(IOError::Graph)?;

        if let Some(out_e) = out_e {
            self.has_out_e.insert(vertex_id.clone());
            self.buffer_edges(out_e, true, &vertex_id, record);
        }
        if let Some(in_e) = in_e {
            self.buffer_edges(in_e, false, &vertex_id, record);
        }
        Ok(())
    }

    /// Decodes a vertex's `properties`: one entry -> its value, several -> an array, meta-
    /// properties -> `_meta`.
    fn vertex_properties(
        &mut self,
        properties: Option<PhysicalValue<'static>>,
        at: &str,
        vertex: &str,
    ) -> PropertyMap {
        let Some(PhysicalValue::Object(fields)) = properties else {
            return PropertyMap::default();
        };
        // Taken out for the loop (it borrows `self` for the report); put back when finished.
        let mut data = std::mem::take(&mut self.vertex_map);
        data.clear();
        let mut meta = PropertyMap::default();
        let mut file_meta = None;
        for (key, field) in fields {
            let key = CompactString::from(key);
            let raw_entries = match field {
                PhysicalValue::Array(items) => items,
                // Tolerated: a single entry object instead of a one-element list.
                single @ PhysicalValue::Object(_) => vec![single],
                _ => {
                    self.report.note(ImportNoteKind::SkippedMalformedEntry, || {
                        format!("{at}: vertex \"{vertex}\", property \"{key}\"")
                    });
                    continue;
                }
            };
            let mut entries = Vec::with_capacity(raw_entries.len());
            for raw in raw_entries {
                if let Some(entry) = self.entry(raw, at, vertex, &key) {
                    entries.push(entry);
                }
            }
            let has_meta = entries.iter().any(|entry| !entry.meta.is_empty());
            let value = match entries.len() {
                0 => continue,
                1 => {
                    let Some(entry) = entries.pop() else {
                        continue;
                    };
                    if has_meta {
                        meta.insert(key.clone(), ElementProperty::Object(Box::new(entry.meta)));
                    }
                    entry.value
                }
                _ => {
                    self.report.note(ImportNoteKind::MultiValuedToArray, || {
                        format!("{at}: vertex \"{vertex}\", property \"{key}\"")
                    });
                    let mut values = Vec::with_capacity(entries.len());
                    let mut metas = Vec::with_capacity(entries.len());
                    for entry in entries {
                        values.push(entry.value);
                        metas.push(ElementProperty::Object(Box::new(entry.meta)));
                    }
                    if has_meta {
                        meta.insert(key.clone(), ElementProperty::Array(metas));
                    }
                    ElementProperty::Array(values)
                }
            };
            if has_meta {
                self.report.note(ImportNoteKind::MetaPropertiesToMeta, || {
                    format!("{at}: vertex \"{vertex}\", property \"{key}\"")
                });
            }
            if key == META_KEY {
                file_meta = Some(value);
            } else {
                data.insert(key, value);
            }
        }
        if !meta.is_empty() {
            if file_meta.is_some() {
                self.report.note(ImportNoteKind::SkippedReservedMeta, || {
                    format!("{at}: vertex \"{vertex}\"")
                });
            }
            data.insert(
                CompactString::from(META_KEY),
                ElementProperty::Object(Box::new(meta)),
            );
        } else if let Some(value) = file_meta {
            data.insert(CompactString::from(META_KEY), value);
        }
        let map = data.finish(&mut self.vertex_shapes);
        self.vertex_map = data;
        map
    }

    /// Decodes one vertex-property entry `{"id": .., "value": .., "properties": {..}}`.
    fn entry(
        &mut self,
        raw: PhysicalValue<'static>,
        at: &str,
        vertex: &str,
        key: &str,
    ) -> Option<Entry> {
        let PhysicalValue::Object(fields) = raw else {
            self.report.note(ImportNoteKind::SkippedMalformedEntry, || {
                format!("{at}: vertex \"{vertex}\", property \"{key}\"")
            });
            return None;
        };
        let mut value = None;
        let mut meta_raw = None;
        for (field, content) in fields {
            match field.as_ref() {
                "value" => value = Some(content),
                "properties" => meta_raw = Some(content),
                _ => {}
            }
        }
        let Some(value) = value else {
            self.report.note(ImportNoteKind::SkippedMalformedEntry, || {
                format!("{at}: vertex \"{vertex}\", property \"{key}\" (no value)")
            });
            return None;
        };
        let mut notes = Vec::new();
        let value = match decode_property(value, &mut notes) {
            Ok(value) => value,
            Err(skip) => {
                self.skip_value(skip, || {
                    format!("{at}: vertex \"{vertex}\", property \"{key}\"")
                });
                return None;
            }
        };
        self.converted(&mut notes, || {
            format!("{at}: vertex \"{vertex}\", property \"{key}\"")
        });
        let mut meta = PropertyMap::default();
        if let Some(PhysicalValue::Object(meta_fields)) = meta_raw {
            for (meta_key, meta_value) in meta_fields {
                match decode_property(meta_value, &mut notes) {
                    Ok(decoded) => {
                        self.converted(&mut notes, || {
                            format!(
                                "{at}: vertex \"{vertex}\", meta-property \"{meta_key}\" of \
                                 \"{key}\""
                            )
                        });
                        meta.insert(CompactString::from(meta_key), decoded);
                    }
                    Err(skip) => {
                        notes.clear();
                        self.skip_value(skip, || {
                            format!(
                                "{at}: vertex \"{vertex}\", meta-property \"{meta_key}\" of \
                                 \"{key}\""
                            )
                        })
                    }
                }
            }
        }
        Some(Entry { value, meta })
    }

    /// Records the conversion notes of a decoded value (`ZoneDropped`, ...) at `sample`.
    fn converted(&mut self, notes: &mut Vec<ImportNoteKind>, sample: impl Fn() -> String) {
        for kind in notes.drain(..) {
            self.report.note(kind, &sample);
        }
    }

    fn skip_value(&mut self, skip: ValueSkip, sample: impl FnOnce() -> String) {
        let reason = skip.reason;
        self.report.note(skip.kind, || match reason {
            Some(reason) => format!("{} ({reason})", sample()),
            None => sample(),
        });
    }

    /// Buffers the edges of an `outE` (`out == true`) or `inE` map of the vertex `this`.
    fn buffer_edges(
        &mut self,
        edges: PhysicalValue<'static>,
        out: bool,
        this: &CompactString,
        record: usize,
    ) {
        let at = self.location(record);
        let PhysicalValue::Object(groups) = edges else {
            self.report.note(ImportNoteKind::SkippedMalformedEntry, || {
                format!(
                    "{at}: vertex \"{this}\", {} is not an object",
                    if out { "outE" } else { "inE" }
                )
            });
            return;
        };
        let other_key = if out { "inV" } else { "outV" };
        for (label, list) in groups {
            let label = CompactString::from(label);
            let PhysicalValue::Array(list) = list else {
                self.report.note(ImportNoteKind::SkippedMalformedEntry, || {
                    format!("{at}: vertex \"{this}\", edge label \"{label}\"")
                });
                continue;
            };
            for edge in list {
                let PhysicalValue::Object(fields) = edge else {
                    self.report.note(ImportNoteKind::SkippedMalformedEntry, || {
                        format!("{at}: vertex \"{this}\", edge label \"{label}\"")
                    });
                    continue;
                };
                let mut id = None;
                let mut other = None;
                let mut props = None;
                for (key, field) in fields {
                    match key.as_ref() {
                        "id" => id = Some(field),
                        "properties" => props = Some(field),
                        key if key == other_key => other = Some(field),
                        _ => {}
                    }
                }
                let id = id.and_then(decode_id);
                let Some(other) = other.and_then(decode_id) else {
                    let shown = id.clone().unwrap_or_default();
                    self.report.note(ImportNoteKind::SkippedMalformedEntry, || {
                        format!("{at}: edge \"{shown}\" has no {other_key}")
                    });
                    continue;
                };
                let mut properties = std::mem::take(&mut self.edge_map);
                properties.clear();
                if let Some(PhysicalValue::Object(prop_fields)) = props {
                    let mut notes = Vec::new();
                    for (key, value) in prop_fields {
                        match decode_property(value, &mut notes) {
                            Ok(decoded) => {
                                let shown = id.clone().unwrap_or_default();
                                self.converted(&mut notes, || {
                                    format!("{at}: edge \"{shown}\", property \"{key}\"")
                                });
                                properties.insert(CompactString::from(key), decoded);
                            }
                            Err(skip) => {
                                notes.clear();
                                let shown = id.clone().unwrap_or_default();
                                self.skip_value(skip, || {
                                    format!("{at}: edge \"{shown}\", property \"{key}\"")
                                });
                            }
                        }
                    }
                }
                let properties_map = properties.finish(&mut self.edge_shapes);
                self.edge_map = properties;
                let properties = properties_map;
                let (from, to) = if out {
                    (this.clone(), other)
                } else {
                    (other, this.clone())
                };
                let pending = PendingEdge {
                    id,
                    label: label.clone(),
                    from,
                    to,
                    properties,
                    record,
                };
                if out {
                    self.out_edges.push(pending);
                } else {
                    self.in_edges.push(pending);
                }
            }
        }
    }

    /// Adds the buffered edges: every `outE` edge, then the `inE` edges that have no `outE` copy.
    fn edges(&mut self) -> Result<(), IOError> {
        // id -> (label, from, to) of the edges added so far.
        let mut seen: HashMap<CompactString, (CompactString, CompactString, CompactString)> =
            HashMap::new();
        let out_edges = std::mem::take(&mut self.out_edges);
        for edge in out_edges {
            if let Some(id) = &edge.id
                && let Some(first) = seen.get(id)
            {
                let at = self.location(edge.record);
                if *first != (edge.label.clone(), edge.from.clone(), edge.to.clone()) {
                    self.report.note(ImportNoteKind::SkippedDuplicateEdge, || {
                        format!("{at}: edge \"{id}\"")
                    });
                }
                continue;
            }
            self.insert_edge(edge, &mut seen)?;
        }
        let in_edges = std::mem::take(&mut self.in_edges);
        for edge in in_edges {
            match &edge.id {
                Some(id) => {
                    if let Some(first) = seen.get(id) {
                        if *first != (edge.label.clone(), edge.from.clone(), edge.to.clone()) {
                            let at = self.location(edge.record);
                            self.report.note(ImportNoteKind::EdgeCopiesDisagree, || {
                                format!("{at}: edge \"{id}\"")
                            });
                        }
                        continue;
                    }
                }
                None => {
                    // Without an id an inE entry cannot be paired: it is a copy whenever its tail
                    // vertex listed its outE.
                    if self.has_out_e.contains(&edge.from) {
                        continue;
                    }
                }
            }
            self.insert_edge(edge, &mut seen)?;
        }
        Ok(())
    }

    fn insert_edge(
        &mut self,
        edge: PendingEdge,
        seen: &mut HashMap<CompactString, (CompactString, CompactString, CompactString)>,
    ) -> Result<(), IOError> {
        let from = self.endpoint(&edge.from);
        let to = self.endpoint(&edge.to);
        let (Some(from), Some(to)) = (from, to) else {
            let at = self.location(edge.record);
            let shown = edge.id.clone().unwrap_or_default();
            let (a, b) = (edge.from.clone(), edge.to.clone());
            self.report.note(ImportNoteKind::SkippedDanglingEdge, || {
                format!("{at}: edge \"{shown}\" from \"{a}\" to \"{b}\"")
            });
            return Ok(());
        };
        if let Some(id) = &edge.id {
            seen.insert(
                id.clone(),
                (edge.label.clone(), edge.from.clone(), edge.to.clone()),
            );
        }
        let label = (!edge.label.is_empty()).then_some(edge.label);
        self.g
            .insert_edge(edge.id, label, from, to, edge.properties)
            .map_err(IOError::Graph)?;
        self.edge_count += 1;
        crate::traversal::memory_budget::tick(self.budget, &*self.g).map_err(IOError::Graph)
    }

    /// An edge endpoint: a vertex of the file, else one already in the target graph.
    fn endpoint(&self, id: &str) -> Option<S::VertexId> {
        self.vertices
            .get(id)
            .copied()
            .or_else(|| self.g.get_vertex_by_id(id))
    }
}

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

/// GraphSON exporter: writes a graph as GraphSON 3.0 in the line form. Build it with
/// [`GraphSONExport::new`].
///
/// Every vertex is one line with both `inE` and `outE` (TinkerGraph builds edges from `inE`), ids
/// are JSON strings, values are typed (`int64` -> `g:Int64`, `float64` -> `g:Double`, `uuid` ->
/// `g:UUID`, `array` -> `g:List`, `object` -> `g:Map`), a label set is joined with `"::"`, an
/// unlabeled vertex or edge is written with TinkerPop's default label (`vertex`/`edge`), and
/// `_meta` is written back as meta-properties when it has the shape an import produces. The file
/// carries no schema.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct GraphSONExport {}

/// How a vertex's `_meta` property is written.
enum MetaPlan<'a> {
    /// No `_meta`, or one written as a plain property.
    Plain,
    /// `_meta` becomes the meta-properties of the keys it names.
    Meta(&'a PropertyMap),
}

impl GraphSONExport {
    /// A new exporter with the default options.
    pub fn new() -> Self {
        Self {}
    }

    /// Exports `graph` as GraphSON to the file at `path`.
    pub fn export_file<S: GraphStorage>(&self, graph: &S, path: &str) -> Result<(), IOError> {
        let f = File::create(path).map_err(|err| IOError::FileWrite {
            path: path.to_string(),
            source: err,
        })?;
        let mut writer = std::io::BufWriter::with_capacity(128 * 1024, f);
        self.export_writer(graph, &mut writer)?;
        std::io::Write::flush(&mut writer).map_err(|source| IOError::FileWrite {
            path: path.to_string(),
            source,
        })
    }

    /// Exports `g` as GraphSON to `writer`, one vertex per line.
    pub fn export_writer<S: GraphStorage, W: std::io::Write>(
        &self,
        g: &S,
        mut writer: W,
    ) -> Result<(), IOError> {
        let mut line = String::new();
        let mut property_id: i64 = 0;
        for vertex in g.vertices() {
            line.clear();
            if write_vertex(g, vertex, &mut property_id, &mut line)
                .map_err(physical_to_io)?
                .is_none()
            {
                continue;
            }
            line.push('\n');
            writer
                .write_all(line.as_bytes())
                .map_err(|source| IOError::FileWrite {
                    path: "<GraphSON output>".to_string(),
                    source,
                })?;
        }
        writer.flush().map_err(|source| IOError::FileWrite {
            path: "<GraphSON output>".to_string(),
            source,
        })
    }
}

fn physical_to_io(err: PhysicalError) -> IOError {
    IOError::FileWrite {
        path: "<GraphSON output>".to_string(),
        source: std::io::Error::other(err.to_string()),
    }
}

/// Writes one vertex line (without the newline); `None` for a vertex without an id.
fn write_vertex<S: GraphStorage>(
    g: &S,
    vertex: S::VertexId,
    property_id: &mut i64,
    out: &mut String,
) -> Result<Option<()>, PhysicalError> {
    let Some(id) = g.vertex_id(vertex) else {
        return Ok(None);
    };
    out.push_str("{\"id\":");
    write_json_string(id, out)?;
    out.push_str(",\"label\":");
    let labels = g.vertex_labels(vertex);
    if labels.is_empty() {
        write_json_string(DEFAULT_VERTEX_LABEL, out)?;
    } else {
        let joined = labels
            .iter()
            .map(CompactString::as_str)
            .collect::<Vec<_>>()
            .join("::");
        write_json_string(&joined, out)?;
    }
    write_edges(g, g.in_edges(vertex).collect(), false, out)?;
    write_edges(g, g.out_edges(vertex).collect(), true, out)?;

    let empty = PropertyMap::default();
    let props = g.vertex_properties(vertex).unwrap_or(&empty);
    let plan = meta_plan(props);
    let mut first = true;
    for (key, value) in props {
        if key == META_KEY && matches!(plan, MetaPlan::Meta(_)) {
            continue;
        }
        out.push_str(if first { ",\"properties\":{" } else { "," });
        first = false;
        write_json_string(key, out)?;
        out.push_str(":[");
        let meta = match plan {
            MetaPlan::Meta(meta) => meta.get(key.as_str()),
            MetaPlan::Plain => None,
        };
        match (meta, value) {
            (Some(ElementProperty::Array(metas)), ElementProperty::Array(values)) => {
                for (i, (item, item_meta)) in values.iter().zip(metas).enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_vp_entry(item, item_meta, property_id, out)?;
                }
            }
            (Some(item_meta), _) => write_vp_entry(value, item_meta, property_id, out)?,
            (None, _) => write_vp_entry(value, &ElementProperty::Null, property_id, out)?,
        }
        out.push(']');
    }
    if !first {
        out.push('}');
    }
    out.push('}');
    Ok(Some(()))
}

/// Writes one vertex-property entry; `meta` is an object of meta-properties (or anything else
/// for none).
fn write_vp_entry(
    value: &ElementProperty,
    meta: &ElementProperty,
    property_id: &mut i64,
    out: &mut String,
) -> Result<(), PhysicalError> {
    write!(
        out,
        "{{\"id\":{{\"@type\":\"g:Int64\",\"@value\":{property_id}}},\"value\":"
    )?;
    *property_id += 1;
    write_typed(value, out)?;
    if let ElementProperty::Object(meta) = meta
        && !meta.is_empty()
    {
        out.push_str(",\"properties\":");
        write_properties_object(meta, out)?;
    }
    out.push('}');
    Ok(())
}

/// `{"k": <typed>, ...}`.
fn write_properties_object(props: &PropertyMap, out: &mut String) -> Result<(), PhysicalError> {
    out.push('{');
    for (i, (key, value)) in props.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_json_string(key, out)?;
        out.push(':');
        write_typed(value, out)?;
    }
    out.push('}');
    Ok(())
}

/// Writes the `inE` (`out == false`) or `outE` map of a vertex, grouped by label.
fn write_edges<S: GraphStorage>(
    g: &S,
    edges: Vec<S::EdgeId>,
    out_direction: bool,
    out: &mut String,
) -> Result<(), PhysicalError> {
    let mut groups: IndexMap<&str, Vec<S::EdgeId>> = IndexMap::new();
    for edge in edges {
        let label = g.edge_label(edge).unwrap_or(DEFAULT_EDGE_LABEL);
        groups.entry(label).or_default().push(edge);
    }
    if groups.is_empty() {
        return Ok(());
    }
    out.push_str(if out_direction {
        ",\"outE\":{"
    } else {
        ",\"inE\":{"
    });
    for (i, (label, list)) in groups.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_json_string(label, out)?;
        out.push_str(":[");
        let mut first = true;
        for edge in list {
            let Some((from, to)) = g.edge_endpoints(*edge) else {
                continue;
            };
            let other = if out_direction { to } else { from };
            let Some(other_id) = g.vertex_id(other) else {
                continue;
            };
            if !first {
                out.push(',');
            }
            first = false;
            out.push('{');
            if let Some(id) = g.edge_id(*edge) {
                out.push_str("\"id\":");
                write_json_string(id, out)?;
                out.push(',');
            }
            out.push_str(if out_direction {
                "\"inV\":"
            } else {
                "\"outV\":"
            });
            write_json_string(other_id, out)?;
            if let Some(props) = g.edge_properties(*edge)
                && !props.is_empty()
            {
                out.push_str(",\"properties\":");
                write_properties_object(props, out)?;
            }
            out.push('}');
        }
        out.push(']');
    }
    out.push('}');
    Ok(())
}

/// Whether `_meta` has exactly the shape an import produces, so that writing it as
/// meta-properties and importing them again gives the same `_meta` back.
fn meta_plan(props: &PropertyMap) -> MetaPlan<'_> {
    let Some(ElementProperty::Object(meta)) = props.get(META_KEY) else {
        return MetaPlan::Plain;
    };
    if meta.is_empty() {
        return MetaPlan::Plain;
    }
    let fits =
        meta.iter().all(|(key, entry)| {
            if key == META_KEY {
                return false;
            }
            let Some(value) = props.get(key.as_str()) else {
                return false;
            };
            match entry {
                // One entry: a non-empty object of meta-properties.
                ElementProperty::Object(object) => !object.is_empty(),
                // Several entries: an array of objects aligned with an array value of length >= 2,
                // at least one of them non-empty.
                ElementProperty::Array(metas) => match value {
                    ElementProperty::Array(values) => values.len() >= 2
                        && metas.len() == values.len()
                        && metas
                            .iter()
                            .all(|m| matches!(m, ElementProperty::Object(_)))
                        && metas.iter().any(
                            |m| matches!(m, ElementProperty::Object(object) if !object.is_empty()),
                        ),
                    _ => false,
                },
                _ => false,
            }
        });
    if fits {
        MetaPlan::Meta(meta)
    } else {
        MetaPlan::Plain
    }
}

/// Writes a value as GraphSON 3.0 typed JSON.
fn write_typed(value: &ElementProperty, out: &mut String) -> Result<(), PhysicalError> {
    match value {
        ElementProperty::Null => out.push_str("null"),
        ElementProperty::Boolean(true) => out.push_str("true"),
        ElementProperty::Boolean(false) => out.push_str("false"),
        ElementProperty::String(s) => write_json_string(s, out)?,
        // Files hold the plain value (D5).
        ElementProperty::Compressed(text) => text.with_text(|s| write_json_string(s, out))?,
        ElementProperty::Deduped(text) => write_json_string(text.as_str(), out)?,
        ElementProperty::Number(number) => match number.n {
            N::NegInt(i) => write!(out, "{{\"@type\":\"g:Int64\",\"@value\":{i}}}")?,
            N::Float(f) => {
                let f = f.into_inner();
                out.push_str("{\"@type\":\"g:Double\",\"@value\":");
                match crate::value::physical::non_finite_text(f) {
                    Some(text) => write_json_string(text, out)?,
                    None => write_float(f, out)?,
                }
                out.push('}');
            }
        },
        ElementProperty::Uuid(u) => {
            out.push_str("{\"@type\":\"g:UUID\",\"@value\":");
            write_json_string(&format_canonical(*u), out)?;
            out.push('}');
        }
        // `gx:OffsetDateTime`, `gx:LocalDate`, ... with the canonical text, which `java.time`
        // parses (`temporal::tags`).
        ElementProperty::Temporal(value) => {
            write!(out, "{{\"@type\":\"{}\",\"@value\":", tags::tag_of(value))?;
            write_json_string(&value.to_string(), out)?;
            out.push('}');
        }
        // A `g:List` of `g:Float` (TinkerGraph loads it; an i8 or bit vector as its f32 values):
        // a list on import, a vector again where the target schema declares one.
        ElementProperty::Vector(vector) => {
            out.push_str("{\"@type\":\"g:List\",\"@value\":[");
            for (i, value) in vector.iter_f32().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str("{\"@type\":\"g:Float\",\"@value\":");
                write_float(crate::value::vector::tagged::short_f64(value), out)?;
                out.push('}');
            }
            out.push_str("]}");
        }
        ElementProperty::Array(items) => {
            out.push_str("{\"@type\":\"g:List\",\"@value\":[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_typed(item, out)?;
            }
            out.push_str("]}");
        }
        // A vertex or an edge value (a query result, never a stored property) is written as
        // TinkerPop's reference element: id and label (and an edge's endpoints), no properties.
        ElementProperty::Vertex(vertex) => {
            out.push_str("{\"@type\":\"g:Vertex\",\"@value\":{\"id\":");
            write_json_string(vertex.id(), out)?;
            out.push_str(",\"label\":");
            write_json_string(vertex.label().unwrap_or(""), out)?;
            out.push_str("}}");
        }
        ElementProperty::Edge(edge) => {
            out.push_str("{\"@type\":\"g:Edge\",\"@value\":{\"id\":");
            write_json_string(edge.id(), out)?;
            for (key, text) in [
                ("label", edge.label().unwrap_or("")),
                ("inVLabel", edge.in_v().label().unwrap_or("")),
                ("outVLabel", edge.out_v().label().unwrap_or("")),
                ("inV", edge.in_v().id()),
                ("outV", edge.out_v().id()),
            ] {
                out.push_str(",\"");
                out.push_str(key);
                out.push_str("\":");
                write_json_string(text, out)?;
            }
            out.push_str("}}");
        }
        ElementProperty::Object(map) => {
            out.push_str("{\"@type\":\"g:Map\",\"@value\":[");
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(key, out)?;
                out.push(',');
                write_typed(item, out)?;
            }
            out.push_str("]}");
        }
        ElementProperty::Map(map) => {
            out.push_str("{\"@type\":\"g:Map\",\"@value\":[");
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_typed(key, out)?;
                out.push(',');
                write_typed(item, out)?;
            }
            out.push_str("]}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(text: &str) -> (TraversalGraph, ImportReport) {
        let mut g = TraversalGraph::new();
        let report = GraphSONImport::new()
            .import_reader(&mut g, text.as_bytes())
            .expect("import");
        (g, report)
    }

    #[test]
    fn typed_scalars_decode() {
        let line = r#"{"id":{"@type":"g:Int32","@value":1},"label":"x","properties":{"a":[{"id":{"@type":"g:Int64","@value":0},"value":{"@type":"g:Int32","@value":7}}],"b":[{"value":{"@type":"g:Double","@value":"NaN"}}]}}"#;
        let (g, report) = import(line);
        let v = GraphStorage::get_vertex_by_id(&g, "1").expect("vertex");
        let props = GraphStorage::vertex_properties(&g, v).expect("props");
        assert_eq!(props.get("a"), Some(&ElementProperty::from(7i64)));
        assert!(
            matches!(props.get("b"), Some(ElementProperty::Number(n)) if n.as_f64().is_some_and(f64::is_nan))
        );
        assert!(!report.has_skips());
    }

    #[test]
    fn local_name_strips_prefix() {
        assert_eq!(local_name("g:Int32"), "Int32");
        assert_eq!(
            local_name("janusgraph:RelationIdentifier"),
            "RelationIdentifier"
        );
        assert_eq!(local_name("plain"), "plain");
    }
}
