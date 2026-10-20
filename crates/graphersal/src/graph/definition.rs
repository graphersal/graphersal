// The catalog of definitions; its public documentation is the module docs of `crate::catalog`.

use std::collections::BTreeMap;
use std::fmt;

use compact_str::CompactString;

use crate::error::{DefinitionError, SchemaError};
use crate::graph::schema::PropertySchema;
use crate::value::physical::{PhysicalValue, lift_untyped, lower, read_json, write_json};
use crate::value::{ElementProperty, PropertyMap};

/// The dialect of a saved query's source this version writes for a query that only reads:
/// Rhai, Graphersal DSL version 1. A query of this dialect is read-only, also when its body has
/// writing steps (they are denied).
pub const QUERY_DIALECT: &str = "graphersal-rhai/1";
/// The dialect of a saved query that may write data (Graphersal DSL version 2): the script
/// layer saves a query in it when the effects inferred from its body are not empty, and in
/// [`QUERY_DIALECT`] otherwise (book `users_guide/saved_queries.md`). A version that runs only
/// [`QUERY_DIALECT`] lists such a query as not runnable.
pub const WRITING_QUERY_DIALECT: &str = "graphersal-rhai/2";
/// The dialect of a saved query written in GQL that only reads: the source is one GQL query
/// (`MATCH ... RETURN ...`), `$name` reads a declared parameter. The script layer saves a GQL
/// query in it when the plan of the query writes nothing, and in [`GQL_WRITING_QUERY_DIALECT`]
/// otherwise (book `users_guide/saved_queries.md`, "Saved queries in GQL"). A version without
/// GQL saved queries lists such a query as not runnable and keeps it unchanged.
pub const GQL_QUERY_DIALECT: &str = "graphersal-gql/1";
/// The dialect of a saved query written in GQL whose plan writes data (`INSERT ...`): a call
/// may write what the plan writes, within the caller's rights, like a query of
/// [`WRITING_QUERY_DIALECT`].
pub const GQL_WRITING_QUERY_DIALECT: &str = "graphersal-gql/2";
/// Keys a definition's payload map never has (format spec 6.3): `kind`, `name` and `flags` are
/// the envelope keys of the exchange form ([`Definition::to_json`]), which puts the payload
/// keys beside them. A stored payload with one of them is invalid.
pub const RESERVED_PAYLOAD_KEYS: [&str; 3] = ["kind", "name", "flags"];

/// The most definitions a [`TraversalGraph`](crate::graph::TraversalGraph) catalog holds.
pub const MAX_DEFINITIONS: usize = 10_000;
/// The longest saved-query name, in bytes.
pub const MAX_QUERY_NAME_BYTES: usize = 128;
/// The longest name of a definition of another (opaque) kind, in bytes.
pub const MAX_DEFINITION_NAME_BYTES: usize = 1024;
/// The longest folder path, in bytes.
pub const MAX_FOLDER_BYTES: usize = 255;
/// The most segments of a folder path.
pub const MAX_FOLDER_SEGMENTS: usize = 8;
/// The longest description, in bytes.
pub const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
/// The longest saved-query source, in bytes.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;
/// The longest `meta` text, in bytes.
pub const MAX_META_BYTES: usize = 64 * 1024;
/// The longest dialect name, in bytes.
pub const MAX_DIALECT_BYTES: usize = 64;
/// The most parameters of one saved query.
pub const MAX_PARAMETERS: usize = 64;
/// The largest payload of an opaque definition, in bytes.
pub const MAX_OPAQUE_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
/// How deep the JSON form of a definition may nest (parameter schemas included).
const MAX_JSON_DEPTH: usize = 128;

/// Names the script scope reserves for the DSL globals, which a script parameter (and so a
/// saved-query parameter) may not shadow.
///
/// `Pop` is reserved as a class name; its bare constants `first` / `last` / `all` / `mixed` are
/// not, because they are likely parameter names (a parameter of that name shadows the constant).
///
/// Every global the script scope pushes (`script::graph_scope`) must be listed here, except
/// those bare constants. The unit test `reserved_param_names_match_graph_scope` (feature
/// `script`) builds the scope and fails when the two drift. It lives here, not in `script/`,
/// so a saved query's parameters are checked the same way without the `script` feature.
pub(crate) const RESERVED_PARAM_NAMES: [&str; 29] = [
    "g",
    "__",
    "_g",
    "P",
    "TextP",
    "V",
    "UUID",
    "Set",
    "Column",
    "keys",
    "values",
    "params",
    "Scope",
    "Cardinality",
    "Direction",
    "Merge",
    "Operator",
    "Barrier",
    "WithOptions",
    "Pop",
    "T",
    "By",
    "Order",
    "Pick",
    "GType",
    "CastPolicy",
    "ProfileType",
    "SchemaMode",
    "DT",
];

/// Rhai's keywords and reserved words (Rhai 1.x, `tokenizer.rs`): neither a function nor a
/// variable may be named so. Superset on purpose (a few are only reserved in some builds).
const RHAI_RESERVED_WORDS: &[&str] = &[
    // Keywords.
    "as",
    "break",
    "catch",
    "const",
    "continue",
    "do",
    "else",
    "export",
    "false",
    "fn",
    "for",
    "global",
    "if",
    "import",
    "in",
    "let",
    "loop",
    "private",
    "return",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "until",
    "while",
    // Keyword functions.
    "Fn",
    "call",
    "curry",
    "debug",
    "eval",
    "is_def_fn",
    "is_def_var",
    "is_shared",
    "print",
    "type_of",
    // Reserved for the future.
    "async",
    "await",
    "case",
    "default",
    "exit",
    "go",
    "goto",
    "is",
    "match",
    "module",
    "new",
    "nil",
    "null",
    "package",
    "protected",
    "public",
    "shared",
    "spawn",
    "static",
    "super",
    "sync",
    "thread",
    "use",
    "var",
    "void",
    "with",
    "yield",
];

/// Whether `name` has the identifier shape Rhai (and an MCP tool name) accepts:
/// `[A-Za-z_][A-Za-z0-9_]*` with a letter before the first digit (Rhai rejects `_1`).
fn is_identifier(name: &str) -> bool {
    let mut letter_seen = false;
    for byte in name.bytes() {
        match byte {
            b'_' => {}
            b'a'..=b'z' | b'A'..=b'Z' => letter_seen = true,
            b'0'..=b'9' if letter_seen => {}
            _ => return false,
        }
    }
    letter_seen
}

/// Why `name` cannot be a Rhai identifier (function or variable), `None` when it can.
fn identifier_problem(name: &str, max_bytes: usize) -> Option<String> {
    if name.is_empty() {
        return Some("it is empty".to_string());
    }
    if name.len() > max_bytes {
        return Some(format!(
            "it is {} bytes long, at most {max_bytes} are allowed",
            name.len()
        ));
    }
    if !is_identifier(name) {
        return Some(
            "it is not an identifier: ASCII letters, digits and '_' only, with a letter before \
             the first digit"
                .to_string(),
        );
    }
    if RHAI_RESERVED_WORDS.contains(&name) {
        return Some("it is a reserved word of the Rhai language".to_string());
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Kind and flags
// ---------------------------------------------------------------------------------------------

/// The kind of a [`Definition`]: one byte. A newtype, not an enum, because a reader must be
/// able to hold every kind, also those a newer version assigned ([`Self::is_known`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
pub struct DefinitionKind(u8);

impl DefinitionKind {
    /// Kind `1`: a saved query ([`QueryDefinition`]).
    pub const QUERY: DefinitionKind = DefinitionKind(1);
    /// Kind `2`: a property index
    /// ([`PropertyIndexDefinition`](crate::graph::property_index::PropertyIndexDefinition)).
    pub const PROPERTY_INDEX: DefinitionKind = DefinitionKind(2);
    /// Kind `3`: a compression rule
    /// ([`CompressionDefinition`](crate::graph::compression::CompressionDefinition)).
    pub const COMPRESSION: DefinitionKind = DefinitionKind(3);
    /// The first kind reserved for hosts (`128`): kinds `128..=255` are never assigned by this
    /// library (see the [module docs](crate::catalog#kind-ranges)).
    pub const HOST_FIRST: DefinitionKind = DefinitionKind(128);
    /// The last kind reserved for hosts (`255`).
    pub const HOST_LAST: DefinitionKind = DefinitionKind(255);

    /// The host kind with this byte, `None` below [`Self::HOST_FIRST`] (kinds `0..=127` belong
    /// to the library). A host kind is stored opaque ([`DefinitionBody::Opaque`]): the host
    /// encodes its payload, for example with `encode_payload` (feature `persist`).
    ///
    /// ```
    /// use graphersal::catalog::DefinitionKind;
    ///
    /// let kind = DefinitionKind::host(200).unwrap();
    /// assert!(kind.is_host());
    /// assert!(!kind.is_known());
    /// assert_eq!(DefinitionKind::host(127), None);
    /// ```
    pub const fn host(code: u8) -> Option<Self> {
        if code >= Self::HOST_FIRST.0 {
            Some(DefinitionKind(code))
        } else {
            None
        }
    }

    /// Whether this kind is reserved for hosts (`128..=255`).
    pub const fn is_host(self) -> bool {
        self.0 >= Self::HOST_FIRST.0
    }

    /// The kind with this byte.
    pub const fn from_code(code: u8) -> Self {
        DefinitionKind(code)
    }

    /// The byte of this kind.
    pub const fn code(self) -> u8 {
        self.0
    }

    /// Whether this version decodes the payload of this kind ([`Self::QUERY`],
    /// [`Self::PROPERTY_INDEX`] and [`Self::COMPRESSION`]); every other kind is kept opaque.
    pub const fn is_known(self) -> bool {
        self.0 == Self::QUERY.0 || self.0 == Self::PROPERTY_INDEX.0 || self.0 == Self::COMPRESSION.0
    }

    /// The name of an assigned kind (`"query"`, `"property_index"`), `None` for an unassigned
    /// one.
    pub const fn name(self) -> Option<&'static str> {
        match self.0 {
            1 => Some("query"),
            2 => Some("property_index"),
            3 => Some("compression"),
            _ => None,
        }
    }

    /// The kind with this [name](Self::name).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "query" => Some(Self::QUERY),
            "property_index" => Some(Self::PROPERTY_INDEX),
            "compression" => Some(Self::COMPRESSION),
            _ => None,
        }
    }
}

/// The name (`query`), or `kind <n>` for an unassigned kind.
impl fmt::Display for DefinitionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "kind {}", self.0),
        }
    }
}

/// The flags byte of a [`Definition`]. Bit 0 is [critical](Self::is_critical); bits 1-7 are
/// reserved: zero when written by this version, preserved when read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
pub struct DefinitionFlags(u8);

impl DefinitionFlags {
    /// No flag set.
    pub const NONE: DefinitionFlags = DefinitionFlags(0);
    /// The bit of [`Self::is_critical`].
    pub const CRITICAL: u8 = 1;

    /// The flags with exactly these bits (reserved bits included, as read).
    pub const fn from_bits(bits: u8) -> Self {
        DefinitionFlags(bits)
    }

    /// All bits.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Critical: a reader that does not know the definition's kind must not write the store,
    /// because it could not keep the definition consistent with the data (an index, ...).
    pub const fn is_critical(self) -> bool {
        self.0 & Self::CRITICAL != 0
    }

    /// The same flags with the critical bit set or cleared.
    pub const fn with_critical(self, critical: bool) -> Self {
        if critical {
            DefinitionFlags(self.0 | Self::CRITICAL)
        } else {
            DefinitionFlags(self.0 & !Self::CRITICAL)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Definition
// ---------------------------------------------------------------------------------------------

/// One entry of the catalog: kind, flags, name and the decoded payload (see the
/// [module docs](crate::catalog)).
///
/// ```
/// use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition, QueryParam};
/// use graphersal::schema::PropertySchema;
///
/// let query = QueryDefinition::new(
///     "fn adults(min_age) { g.v().has(\"age\", P.gte(min_age)) }",
/// )
/// .with_folder("people")
/// .with_param("min_age", QueryParam::new(PropertySchema::integer()).with_default(18));
/// let definition = Definition::query("adults", query);
/// assert_eq!(definition.kind(), DefinitionKind::QUERY);
/// definition.validate().unwrap();
/// let text = definition.to_json();
/// assert_eq!(Definition::from_json(&text).unwrap(), definition);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    kind: DefinitionKind,
    flags: DefinitionFlags,
    name: String,
    body: DefinitionBody,
}

/// The payload of a [`Definition`]: decoded for a known kind, opaque bytes otherwise.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DefinitionBody {
    /// A saved query (kind [`DefinitionKind::QUERY`]).
    Query(QueryDefinition),
    /// A compression rule (kind [`DefinitionKind::COMPRESSION`]).
    Compression(crate::graph::compression::CompressionDefinition),
    /// A property index (kind [`DefinitionKind::PROPERTY_INDEX`]).
    PropertyIndex(crate::graph::property_index::PropertyIndexDefinition),
    /// A payload this version does not decode (an unknown kind, or a known kind whose payload
    /// could not be decoded): its bytes exactly as read, written back unchanged.
    Opaque(Vec<u8>),
}

impl Definition {
    /// A definition from its parts (the constructors of the kinds).
    pub(crate) fn from_parts(
        kind: DefinitionKind,
        flags: DefinitionFlags,
        name: String,
        body: DefinitionBody,
    ) -> Self {
        Definition {
            kind,
            flags,
            name,
            body,
        }
    }

    /// A saved query named `name` (kind [`DefinitionKind::QUERY`], no flags).
    pub fn query(name: impl Into<String>, query: QueryDefinition) -> Self {
        Definition {
            kind: DefinitionKind::QUERY,
            flags: DefinitionFlags::NONE,
            name: name.into(),
            body: DefinitionBody::Query(query),
        }
    }

    /// A definition kept opaque: `payload` is the encoded payload exactly as read (a store
    /// written by a newer version), written back unchanged.
    pub fn opaque(
        kind: DefinitionKind,
        flags: DefinitionFlags,
        name: impl Into<String>,
        payload: Vec<u8>,
    ) -> Self {
        Definition {
            kind,
            flags,
            name: name.into(),
            body: DefinitionBody::Opaque(payload),
        }
    }

    /// The same definition with these flags.
    pub fn with_flags(mut self, flags: DefinitionFlags) -> Self {
        self.flags = flags;
        self
    }

    /// The kind.
    pub fn kind(&self) -> DefinitionKind {
        self.kind
    }

    /// The flags.
    pub fn flags(&self) -> DefinitionFlags {
        self.flags
    }

    /// The name, unique per kind.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The payload.
    pub fn body(&self) -> &DefinitionBody {
        &self.body
    }

    /// The saved query, when this is a decoded one.
    pub fn as_query(&self) -> Option<&QueryDefinition> {
        match &self.body {
            DefinitionBody::Query(query) => Some(query),
            DefinitionBody::Compression(_)
            | DefinitionBody::PropertyIndex(_)
            | DefinitionBody::Opaque(_) => None,
        }
    }

    /// The mutable saved query, when this is a decoded one.
    pub fn as_query_mut(&mut self) -> Option<&mut QueryDefinition> {
        match &mut self.body {
            DefinitionBody::Query(query) => Some(query),
            DefinitionBody::Compression(_)
            | DefinitionBody::PropertyIndex(_)
            | DefinitionBody::Opaque(_) => None,
        }
    }

    /// The opaque payload bytes, when this definition is kept opaque.
    pub fn opaque_payload(&self) -> Option<&[u8]> {
        match &self.body {
            DefinitionBody::Opaque(bytes) => Some(bytes),
            DefinitionBody::Query(_)
            | DefinitionBody::Compression(_)
            | DefinitionBody::PropertyIndex(_) => None,
        }
    }

    /// Whether the payload is kept opaque.
    pub fn is_opaque(&self) -> bool {
        matches!(self.body, DefinitionBody::Opaque(_))
    }

    /// Checks every rule a definition must satisfy to be stored (see the [module docs](crate::catalog)):
    /// for a saved query its name (an identifier of 1 to [`MAX_QUERY_NAME_BYTES`] bytes, not a
    /// Rhai keyword), folder ([`MAX_FOLDER_SEGMENTS`] non-empty trimmed segments, at most
    /// [`MAX_FOLDER_BYTES`] bytes), the sizes of description, dialect, source and `meta`, and
    /// its parameters (identifier names that are not DSL globals, at most [`MAX_PARAMETERS`],
    /// every default valid under its schema). An opaque definition: a name of 1 to
    /// [`MAX_DEFINITION_NAME_BYTES`] bytes and a payload of at most
    /// [`MAX_OPAQUE_PAYLOAD_BYTES`]. The source of a saved query is NOT compiled here.
    pub fn validate(&self) -> Result<(), DefinitionError> {
        match &self.body {
            DefinitionBody::Query(query) => {
                if let Some(reason) = identifier_problem(&self.name, MAX_QUERY_NAME_BYTES) {
                    return Err(DefinitionError::InvalidName {
                        kind: self.kind.to_string(),
                        name: self.name.clone(),
                        reason,
                    });
                }
                query.validate(&self.name)
            }
            DefinitionBody::Compression(rule) => {
                if let Some(reason) = identifier_problem(&self.name, MAX_QUERY_NAME_BYTES) {
                    return Err(DefinitionError::InvalidName {
                        kind: self.kind.to_string(),
                        name: self.name.clone(),
                        reason,
                    });
                }
                rule.validate(&self.name)
            }
            DefinitionBody::PropertyIndex(index) => {
                if let Some(reason) = identifier_problem(&self.name, MAX_QUERY_NAME_BYTES) {
                    return Err(DefinitionError::InvalidName {
                        kind: self.kind.to_string(),
                        name: self.name.clone(),
                        reason,
                    });
                }
                index.validate(&self.name)
            }
            DefinitionBody::Opaque(payload) => {
                let reason = if self.name.is_empty() {
                    Some("it is empty".to_string())
                } else if self.name.len() > MAX_DEFINITION_NAME_BYTES {
                    Some(format!(
                        "it is {} bytes long, at most {MAX_DEFINITION_NAME_BYTES} are allowed",
                        self.name.len()
                    ))
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return Err(DefinitionError::InvalidName {
                        kind: self.kind.to_string(),
                        name: self.name.clone(),
                        reason,
                    });
                }
                if payload.len() > MAX_OPAQUE_PAYLOAD_BYTES {
                    return Err(DefinitionError::InvalidField {
                        name: self.name.clone(),
                        field: "payload".to_string(),
                        reason: format!(
                            "it is {} bytes long, at most {MAX_OPAQUE_PAYLOAD_BYTES} are allowed",
                            payload.len()
                        ),
                    });
                }
                Ok(())
            }
        }
    }

    /// The exchange form as a value: an object with `kind`, `name`, `flags` (omitted when 0)
    /// and, for a saved query, its payload keys with `params` as an object (keys in the order
    /// of [`QueryDefinition::to_payload`], unknown keys last); for an opaque definition the
    /// numeric `kind` and `payload` as lowercase hex.
    pub fn to_value(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        match &self.body {
            DefinitionBody::Query(query) => {
                map.insert("kind", kind_value(self.kind));
                map.insert("name", ElementProperty::from(self.name.as_str()));
                if self.flags != DefinitionFlags::NONE {
                    map.insert("flags", ElementProperty::from(self.flags.0 as i64));
                }
                query.write_fields(&mut map, true);
            }
            DefinitionBody::Compression(rule) => {
                map.insert("kind", kind_value(self.kind));
                map.insert("name", ElementProperty::from(self.name.as_str()));
                if self.flags != DefinitionFlags::NONE {
                    map.insert("flags", ElementProperty::from(self.flags.0 as i64));
                }
                rule.write_fields(&mut map);
            }
            DefinitionBody::PropertyIndex(index) => {
                map.insert("kind", kind_value(self.kind));
                map.insert("name", ElementProperty::from(self.name.as_str()));
                if self.flags != DefinitionFlags::NONE {
                    map.insert("flags", ElementProperty::from(self.flags.0 as i64));
                }
                index.write_fields(&mut map);
            }
            DefinitionBody::Opaque(payload) => {
                map.insert("kind", ElementProperty::from(self.kind.0 as i64));
                map.insert("name", ElementProperty::from(self.name.as_str()));
                if self.flags != DefinitionFlags::NONE {
                    map.insert("flags", ElementProperty::from(self.flags.0 as i64));
                }
                map.insert("payload", ElementProperty::from(hex(payload)));
            }
        }
        ElementProperty::Object(Box::new(map))
    }

    /// Reads the exchange form of [`Self::to_value`]. A `payload` key makes the definition
    /// opaque; otherwise `kind` must be `"query"` (or `1`). Unknown keys of a saved query are
    /// kept ([`QueryDefinition::extra`]); a known key of the wrong type is an error. Nothing is
    /// [validated](Self::validate) beyond the form.
    pub fn from_value(value: &ElementProperty) -> Result<Definition, DefinitionError> {
        let map = value
            .as_object()
            .ok_or_else(|| malformed("", "a definition must be an object"))?;
        let kind = match map.get("kind") {
            Some(ElementProperty::String(name)) => DefinitionKind::from_name(name)
                .ok_or_else(|| malformed("/kind", format!("unknown kind name '{name}'")))?,
            Some(number @ ElementProperty::Number(_)) => number
                .as_i64()
                .and_then(|code| u8::try_from(code).ok())
                .map(DefinitionKind)
                .ok_or_else(|| malformed("/kind", "a kind number is 0 to 255"))?,
            Some(_) => return Err(malformed("/kind", "must be a kind name or number")),
            None => return Err(malformed("", "'kind' is missing")),
        };
        let name = match map.get("name") {
            Some(ElementProperty::String(name)) => name.to_string(),
            Some(_) => return Err(malformed("/name", "must be a string")),
            None => return Err(malformed("", "'name' is missing")),
        };
        let flags = match map.get("flags") {
            None => DefinitionFlags::NONE,
            Some(value) => value
                .as_i64()
                .and_then(|bits| u8::try_from(bits).ok())
                .map(DefinitionFlags)
                .ok_or_else(|| malformed("/flags", "flags are a number from 0 to 255"))?,
        };
        if let Some(payload) = map.get("payload") {
            let text = payload
                .as_str()
                .ok_or_else(|| malformed("/payload", "must be a hex string"))?;
            let bytes = unhex(text).ok_or_else(|| {
                malformed(
                    "/payload",
                    "must be lowercase or uppercase hex, two digits per byte",
                )
            })?;
            return Ok(Definition::opaque(kind, flags, name, bytes));
        }
        if kind == DefinitionKind::COMPRESSION {
            let rule = crate::graph::compression::CompressionDefinition::read_fields(
                map,
                &["kind", "name", "flags"],
            )?;
            return Ok(Definition {
                kind,
                flags,
                name,
                body: DefinitionBody::Compression(rule),
            });
        }
        if kind == DefinitionKind::PROPERTY_INDEX {
            let index = crate::graph::property_index::PropertyIndexDefinition::read_fields(
                map,
                &["kind", "name", "flags"],
            )?;
            return Ok(Definition {
                kind,
                flags,
                name,
                body: DefinitionBody::PropertyIndex(index),
            });
        }
        if kind != DefinitionKind::QUERY {
            return Err(malformed(
                "/kind",
                format!(
                    "a definition of {kind} needs its 'payload' (this version decodes queries, property indexes and compression rules)"
                ),
            ));
        }
        let query = QueryDefinition::read_fields(map, &["kind", "name", "flags"], true)?;
        Ok(Definition {
            kind,
            flags,
            name,
            body: DefinitionBody::Query(query),
        })
    }

    /// The exchange form as compact JSON text ([`Self::to_value`] through the physical codec;
    /// a `uuid` value in a default or an unknown key is written as its canonical string).
    pub fn to_json(&self) -> String {
        json_text(&self.to_value())
    }

    /// Reads the JSON text of [`Self::to_json`] (see [`Self::from_value`]).
    pub fn from_json(text: &str) -> Result<Definition, DefinitionError> {
        Definition::from_value(&parse_json(text)?)
    }
}

/// The exchange form of a whole catalog: `{"definitions": [..]}`, each entry as
/// [`Definition::to_json`] writes it, in the given order.
pub fn catalog_to_json<'a>(definitions: impl IntoIterator<Item = &'a Definition>) -> String {
    let items = definitions.into_iter().map(Definition::to_value).collect();
    let mut map = PropertyMap::default();
    map.insert("definitions", ElementProperty::Array(items));
    json_text(&ElementProperty::Object(Box::new(map)))
}

/// Reads the text of [`catalog_to_json`]. Duplicates (same kind and name) are an error; an
/// unknown top-level key is ignored. Nothing is [validated](Definition::validate) beyond the
/// form.
pub fn catalog_from_json(text: &str) -> Result<Vec<Definition>, DefinitionError> {
    let value = parse_json(text)?;
    let map = value
        .as_object()
        .ok_or_else(|| malformed("", "a catalog must be an object"))?;
    let items = match map.get("definitions") {
        Some(ElementProperty::Array(items)) => items,
        Some(_) => return Err(malformed("/definitions", "must be an array")),
        None => return Err(malformed("", "'definitions' is missing")),
    };
    let mut out: Vec<Definition> = Vec::with_capacity(items.len());
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let definition = Definition::from_value(item).map_err(|err| match err {
            DefinitionError::Malformed { reason } => DefinitionError::Malformed {
                reason: format!("/definitions/{index}{reason}"),
            },
            other => other,
        })?;
        if !seen.insert((definition.kind, definition.name.clone())) {
            return Err(malformed(
                &format!("/definitions/{index}"),
                format!("a second {} named '{}'", definition.kind, definition.name),
            ));
        }
        out.push(definition);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Payload codec
// ---------------------------------------------------------------------------------------------

/// Encodes a definition payload the way the library's own kinds are stored (format spec 6.3):
/// a property map, `uvar count | (key | value)*`, insertion order kept, values in the persist
/// codec's tagged encoding (every [`ElementProperty`] type, `uuid` included, round-trips
/// exactly). Meant for a host kind ([`DefinitionKind::host`]), whose bytes the library stores
/// opaque.
///
/// # Errors
///
/// [`DefinitionError::Malformed`] for a key of [`RESERVED_PAYLOAD_KEYS`] or a value that is
/// not storable (a map with a non-string key, nesting deeper than the encoder's bound).
///
/// ```
/// use graphersal::catalog::{decode_payload, encode_payload};
/// use graphersal::catalog::{Definition, DefinitionFlags, DefinitionKind};
/// use graphersal::prelude::{ElementProperty, PropertyMap};
///
/// let mut payload = PropertyMap::default();
/// payload.insert("label", ElementProperty::from("person"));
/// payload.insert("keys", ElementProperty::from(vec!["name"]));
/// let bytes = encode_payload(&payload).unwrap();
/// let kind = DefinitionKind::host(200).unwrap();
/// let definition = Definition::opaque(kind, DefinitionFlags::NONE, "person_name", bytes);
/// assert_eq!(decode_payload(definition.opaque_payload().unwrap()).unwrap(), payload);
/// ```
#[cfg(feature = "persist")]
pub fn encode_payload(payload: &PropertyMap) -> Result<Vec<u8>, DefinitionError> {
    crate::persist::encode_payload(payload).map_err(|reason| malformed("", reason))
}

/// Decodes a payload written by [`encode_payload`]. Hostile-input safe: every count and length
/// is checked against the remaining bytes before anything is allocated, values nest at most
/// 128 levels (the default `traversal.max_value_depth`).
///
/// # Errors
///
/// [`DefinitionError::Malformed`] with the byte offset within the payload: truncated or
/// invalid bytes, trailing bytes after the map, a repeated key, a reserved key
/// ([`RESERVED_PAYLOAD_KEYS`]), nesting too deep.
#[cfg(feature = "persist")]
pub fn decode_payload(bytes: &[u8]) -> Result<PropertyMap, DefinitionError> {
    crate::persist::decode_payload(bytes, crate::value::depth::DEFAULT_MAX_VALUE_DEPTH)
        .map_err(|err| malformed("", format!("payload byte {}: {}", err.offset, err.reason)))
}

// ---------------------------------------------------------------------------------------------
// Saved queries
// ---------------------------------------------------------------------------------------------

/// A saved query: one Rhai function (or one GQL query) in the database, called by name with
/// named, typed parameters (`book/src/users_guide/saved_queries.md`). Built with [`Self::new`]
/// and the `with_*` methods; stored as a [`Definition::query`].
///
/// The [dialect](Self::dialect) names the language of the source ([`Self::language`]): for
/// [`QUERY_DIALECT`] and [`WRITING_QUERY_DIALECT`] the source is the text of one Rhai function
/// named like the query whose parameters are [`Self::params`] in that order; for
/// [`GQL_QUERY_DIALECT`] and [`GQL_WRITING_QUERY_DIALECT`] it is one GQL query whose `$name`
/// references are exactly [`Self::params`]. That is checked by the script layer (when defined
/// through the DSL and when called), not by [`Definition::validate`].
///
/// What a call may write is derived from the source and the dialect by the script layer, never
/// stored (there is no `writes` field): a query of a read-only dialect runs under a read-only
/// authorizer.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryDefinition {
    folder: Option<String>,
    description: Option<String>,
    dialect: String,
    source: String,
    params: Vec<(String, QueryParam)>,
    meta: Option<String>,
    extra: PropertyMap,
    /// The `params` value of a stored payload this version cannot read (a newer Graphersal
    /// wrote it) and why: kept and written back unchanged, the query is not run.
    unreadable_params: Option<Box<(ElementProperty, String)>>,
}

impl QueryDefinition {
    /// A saved query with this source: root folder, no description, dialect
    /// [`QUERY_DIALECT`], no parameters.
    pub fn new(source: impl Into<String>) -> Self {
        QueryDefinition {
            folder: None,
            description: None,
            dialect: QUERY_DIALECT.to_string(),
            source: source.into(),
            params: Vec::new(),
            meta: None,
            extra: PropertyMap::default(),
            unreadable_params: None,
        }
    }

    /// Builder: the folder (`"reports/sales"`); an empty text is the root.
    pub fn with_folder(mut self, folder: impl Into<String>) -> Self {
        self.set_folder(Some(folder.into()));
        self
    }

    /// Builder: the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.set_description(Some(description.into()));
        self
    }

    /// Builder: the dialect of the source (default [`QUERY_DIALECT`]).
    pub fn with_dialect(mut self, dialect: impl Into<String>) -> Self {
        self.dialect = dialect.into();
        self
    }

    /// Builder: adds the parameter `name` at the end, or replaces the one of that name in its
    /// place.
    pub fn with_param(mut self, name: impl Into<String>, param: QueryParam) -> Self {
        // Parameters this version could not read are kept as read: none is added.
        if self.unreadable_params.is_some() {
            return self;
        }
        let name = name.into();
        match self.params.iter_mut().find(|(n, _)| *n == name) {
            Some((_, slot)) => *slot = param,
            None => self.params.push((name, param)),
        }
        self
    }

    /// Builder: free text, stored and never interpreted.
    pub fn with_meta(mut self, meta: impl Into<String>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    /// Sets the folder; `None` or an empty text is the root.
    pub fn set_folder(&mut self, folder: Option<String>) {
        self.folder = folder.filter(|f| !f.is_empty());
    }

    /// Sets the description; `None` or an empty text removes it.
    pub fn set_description(&mut self, description: Option<String>) {
        self.description = description.filter(|d| !d.is_empty());
    }

    /// The folder, `None` for the root.
    pub fn folder(&self) -> Option<&str> {
        self.folder.as_deref()
    }

    /// The description, if any.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The dialect of the source.
    pub fn dialect(&self) -> &str {
        &self.dialect
    }

    /// The language of the source, from the [dialect](Self::dialect): its name before the
    /// version, without the `graphersal-` prefix (`"rhai"` for [`QUERY_DIALECT`] and
    /// [`WRITING_QUERY_DIALECT`], `"gql"` for [`GQL_QUERY_DIALECT`] and
    /// [`GQL_WRITING_QUERY_DIALECT`]; an unknown dialect gives its own name).
    pub fn language(&self) -> &str {
        let name = self
            .dialect
            .split_once('/')
            .map_or(self.dialect.as_str(), |(name, _)| name);
        name.strip_prefix("graphersal-").unwrap_or(name)
    }

    /// The source: the text of one Rhai function, or one GQL query (see [`Self::language`]).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The parameters, in declaration order (the order of the function's signature).
    pub fn params(&self) -> &[(String, QueryParam)] {
        &self.params
    }

    /// The parameter `name`.
    pub fn param(&self, name: &str) -> Option<&QueryParam> {
        self.params.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }

    /// The free `meta` text, if any.
    pub fn meta(&self) -> Option<&str> {
        self.meta.as_deref()
    }

    /// Why this version cannot run the query, `None` when it can: its stored parameters use
    /// something this version does not know (a newer Graphersal wrote them, for example a
    /// schema keyword). Such a query is kept, listed and written back unchanged; a call is
    /// refused with this reason. [`Self::params`] is empty for it.
    pub fn unsupported_reason(&self) -> Option<String> {
        self.unreadable_params.as_ref().map(|unreadable| {
            format!(
                "its parameters cannot be read by this version (a newer Graphersal wrote \
                 them?): {}; the query is kept unchanged and not run",
                unreadable.1
            )
        })
    }

    /// The payload keys this version does not know, kept as read and written back after the
    /// known ones (a definition written by a newer version keeps them through this one).
    pub fn extra(&self) -> &PropertyMap {
        &self.extra
    }

    /// The payload property map (format spec, section "Definitions"), in this key order:
    ///
    /// | Key | Value |
    /// |---|---|
    /// | `folder` | string, absent = root |
    /// | `description` | string, absent = none |
    /// | `dialect` | string ([`QUERY_DIALECT`], [`WRITING_QUERY_DIALECT`], [`GQL_QUERY_DIALECT`], [`GQL_WRITING_QUERY_DIALECT`]) |
    /// | `source` | string: the Rhai function text, or the GQL query text |
    /// | `params` | string: canonical JSON, name -> `{schema, default?}` ([`Self::params_to_json`]) |
    /// | `meta` | string, absent = none |
    ///
    /// followed by the [unknown keys](Self::extra) in their order.
    pub fn to_payload(&self) -> PropertyMap {
        let mut map = PropertyMap::default();
        self.write_fields(&mut map, false);
        map
    }

    /// Reads a payload property map (the inverse of [`Self::to_payload`]): unknown keys are
    /// kept, a known key of the wrong type is an error, unknown keys inside a parameter object
    /// are kept as well. Not [validated](Definition::validate).
    pub fn from_payload(payload: &PropertyMap) -> Result<QueryDefinition, DefinitionError> {
        Self::read_fields(payload, &[], false)
    }

    /// The canonical JSON text of the parameters: an object, parameter name -> `{"schema":
    /// <canonical node>, "default": <value>}` (default omitted when there is none), in
    /// declaration order.
    pub fn params_to_json(&self) -> String {
        json_text(&self.params_value())
    }

    fn params_value(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        for (name, param) in &self.params {
            map.insert(name.as_str(), param.to_value());
        }
        ElementProperty::Object(Box::new(map))
    }

    /// Writes the payload keys (`params` as an object when `params_object`, else as its JSON
    /// text), then the unknown keys.
    fn write_fields(&self, map: &mut PropertyMap, params_object: bool) {
        if let Some(folder) = &self.folder {
            map.insert("folder", ElementProperty::from(folder.as_str()));
        }
        if let Some(description) = &self.description {
            map.insert("description", ElementProperty::from(description.as_str()));
        }
        map.insert("dialect", ElementProperty::from(self.dialect.as_str()));
        map.insert("source", ElementProperty::from(self.source.as_str()));
        let params = match &self.unreadable_params {
            Some(unreadable) => unreadable.0.clone(),
            None if params_object => self.params_value(),
            None => ElementProperty::from(self.params_to_json()),
        };
        map.insert("params", params);
        if let Some(meta) = &self.meta {
            map.insert("meta", ElementProperty::from(meta.as_str()));
        }
        for (key, value) in &self.extra {
            map.0.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    /// Reads the known keys of `map` (skipping `skip`, the envelope keys of the exchange form)
    /// and keeps the rest as unknown keys. `params` is an object in the exchange form
    /// (`params_object`), a JSON string in the payload (either is accepted in both).
    fn read_fields(
        map: &PropertyMap,
        skip: &[&str],
        params_object: bool,
    ) -> Result<QueryDefinition, DefinitionError> {
        let text = |key: &str| -> Result<Option<String>, DefinitionError> {
            match map.get(key) {
                None => Ok(None),
                Some(ElementProperty::String(text)) => Ok(Some(text.to_string())),
                Some(_) => Err(malformed(&format!("/{key}"), "must be a string")),
            }
        };
        let mut query = QueryDefinition::new(
            text("source")?.ok_or_else(|| malformed("", "'source' is missing"))?,
        );
        query.set_folder(text("folder")?);
        query.set_description(text("description")?);
        if let Some(dialect) = text("dialect")? {
            query.dialect = dialect;
        }
        query.meta = text("meta")?;
        // The parameters of a stored payload that this version cannot read (a newer one wrote
        // them) keep the definition: they are kept as read and the query is not run.
        let keep_unreadable = |value: &ElementProperty, err: DefinitionError| {
            if params_object {
                Err(err)
            } else {
                Ok(Box::new((value.clone(), err.to_string())))
            }
        };
        query.params = match map.get("params") {
            None => Vec::new(),
            Some(raw @ ElementProperty::String(json)) => {
                let parsed = parse_json(json)
                    .map_err(|err| match err {
                        DefinitionError::Malformed { reason } => malformed("/params", reason),
                        other => other,
                    })
                    .and_then(|value| parse_params(&value, false));
                match parsed {
                    Ok(params) => params,
                    Err(err) => {
                        query.unreadable_params = Some(keep_unreadable(raw, err)?);
                        Vec::new()
                    }
                }
            }
            Some(value @ ElementProperty::Object(_)) => match parse_params(value, false) {
                Ok(params) => params,
                Err(err) => {
                    query.unreadable_params = Some(keep_unreadable(value, err)?);
                    Vec::new()
                }
            },
            Some(_) => {
                let expected = if params_object {
                    "must be an object"
                } else {
                    "must be the JSON text of an object"
                };
                return Err(malformed("/params", expected));
            }
        };
        const KNOWN: [&str; 6] = [
            "folder",
            "description",
            "dialect",
            "source",
            "params",
            "meta",
        ];
        for (key, value) in map {
            if !KNOWN.contains(&key.as_str()) && !skip.contains(&key.as_str()) {
                query.extra.insert(key.clone(), value.clone());
            }
        }
        Ok(query)
    }

    /// The rules of [`Definition::validate`] for a saved query named `name`.
    fn validate(&self, name: &str) -> Result<(), DefinitionError> {
        if let Some(folder) = &self.folder
            && let Some(reason) = folder_problem(folder)
        {
            return Err(DefinitionError::InvalidFolder {
                name: name.to_string(),
                folder: folder.clone(),
                reason,
            });
        }
        let field = |field: &str, reason: String| DefinitionError::InvalidField {
            name: name.to_string(),
            field: field.to_string(),
            reason,
        };
        let too_long =
            |len: usize, max: usize| format!("it is {len} bytes long, at most {max} are allowed");
        if let Some(description) = &self.description
            && description.len() > MAX_DESCRIPTION_BYTES
        {
            return Err(field(
                "description",
                too_long(description.len(), MAX_DESCRIPTION_BYTES),
            ));
        }
        if self.dialect.is_empty() {
            return Err(field("dialect", "it is empty".to_string()));
        }
        if self.dialect.len() > MAX_DIALECT_BYTES {
            return Err(field(
                "dialect",
                too_long(self.dialect.len(), MAX_DIALECT_BYTES),
            ));
        }
        if self.source.trim().is_empty() {
            return Err(field("source", "it is empty".to_string()));
        }
        if self.source.len() > MAX_SOURCE_BYTES {
            return Err(field(
                "source",
                too_long(self.source.len(), MAX_SOURCE_BYTES),
            ));
        }
        if let Some(meta) = &self.meta
            && meta.len() > MAX_META_BYTES
        {
            return Err(field("meta", too_long(meta.len(), MAX_META_BYTES)));
        }
        if let Some(key) = self
            .extra
            .keys()
            .find(|key| RESERVED_PAYLOAD_KEYS.contains(&key.as_str()))
        {
            return Err(field(
                "payload",
                format!(
                    "the payload key '{key}' is reserved: 'kind', 'name' and 'flags' are the \
                     envelope keys of the exchange form and never payload keys"
                ),
            ));
        }
        if !storable(&ElementProperty::Object(Box::new(self.extra.clone()))) {
            return Err(field(
                "payload",
                "an unknown key holds a vertex, an edge or a map with non-string keys".to_string(),
            ));
        }
        if self.params.len() > MAX_PARAMETERS {
            return Err(DefinitionError::InvalidParameter {
                parameter: self.params[MAX_PARAMETERS].0.clone(),
                reason: format!("a saved query has at most {MAX_PARAMETERS} parameters"),
            });
        }
        for (index, (parameter, param)) in self.params.iter().enumerate() {
            let invalid = |reason: String| DefinitionError::InvalidParameter {
                parameter: parameter.clone(),
                reason,
            };
            if let Some(reason) = identifier_problem(parameter, MAX_QUERY_NAME_BYTES) {
                return Err(invalid(format!("the name is invalid: {reason}")));
            }
            if RESERVED_PARAM_NAMES.contains(&parameter.as_str()) {
                return Err(invalid(
                    "the name is a global of the DSL (g, __, P, T, ...), which a parameter \
                     may not shadow"
                        .to_string(),
                ));
            }
            if self.params[..index]
                .iter()
                .any(|(other, _)| other == parameter)
            {
                return Err(invalid("the name is declared twice".to_string()));
            }
            param.validate(name, parameter).map_err(invalid)?;
        }
        Ok(())
    }
}

/// Why `folder` is not a valid folder path, `None` when it is.
fn folder_problem(folder: &str) -> Option<String> {
    if folder.len() > MAX_FOLDER_BYTES {
        return Some(format!(
            "it is {} bytes long, at most {MAX_FOLDER_BYTES} are allowed",
            folder.len()
        ));
    }
    if folder.starts_with('/') || folder.ends_with('/') {
        return Some("a folder path has no leading or trailing '/'".to_string());
    }
    let segments: Vec<&str> = folder.split('/').collect();
    if segments.len() > MAX_FOLDER_SEGMENTS {
        return Some(format!(
            "it has {} segments, at most {MAX_FOLDER_SEGMENTS} are allowed",
            segments.len()
        ));
    }
    for segment in segments {
        if segment.is_empty() {
            return Some("a segment is empty ('//')".to_string());
        }
        if segment.trim() != segment {
            return Some(format!("the segment '{segment}' has surrounding spaces"));
        }
        if segment.chars().any(char::is_control) {
            return Some(format!("the segment '{segment}' has a control character"));
        }
    }
    None
}

/// One parameter of a saved query: its schema (the node format of the graph schema,
/// [`PropertySchema`]) and an optional default.
///
/// The default sits beside the schema: a `default` keyword inside the parameter's schema is an
/// annotation, while the parameter's own default IS substituted when a call omits the argument;
/// a parameter without one is required. A
/// default given as a value the schema coerces on write (a canonical uuid string for
/// `format: "uuid"`, an `int64` for `number`) is stored coerced.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryParam {
    schema: PropertySchema,
    default: Option<ElementProperty>,
    extra: PropertyMap,
}

impl QueryParam {
    /// A required parameter with this schema.
    pub fn new(schema: PropertySchema) -> Self {
        QueryParam {
            schema,
            default: None,
            extra: PropertyMap::default(),
        }
    }

    /// Builder: the default (coerced to the schema where a write would coerce it; checked
    /// against the schema by [`Definition::validate`]).
    pub fn with_default(mut self, default: impl Into<ElementProperty>) -> Self {
        let default = default.into();
        let coerced =
            crate::graph::schema::coerce::coerce_to_declared(default.clone(), &self.schema)
                .unwrap_or(default);
        self.default = Some(coerced);
        self
    }

    /// The schema of the parameter.
    pub fn schema(&self) -> &PropertySchema {
        &self.schema
    }

    /// The default, `None` for a required parameter. `Some(Null)` is a default of `null`.
    pub fn default_value(&self) -> Option<&ElementProperty> {
        self.default.as_ref()
    }

    /// Whether a call must pass this parameter (it has no default).
    pub fn is_required(&self) -> bool {
        self.default.is_none()
    }

    /// Keys of the parameter object this version does not know, kept as read.
    pub fn extra(&self) -> &PropertyMap {
        &self.extra
    }

    /// Reads a parameter declaration as a user writes it, strictly: an object
    /// `{"schema": <node>, "default": <value>}` (`default` optional, no other key), or the
    /// short form, a bare type name (`"integer"`, `"number"`, `"string"`, `"boolean"`,
    /// `"array"`, `"object"`, `"null"`, or `"uuid"` for a string with `format: "uuid"`).
    /// `parameter` names it in errors.
    pub fn from_value(
        parameter: &str,
        value: &ElementProperty,
    ) -> Result<QueryParam, DefinitionError> {
        parse_param(parameter, value, true)
    }

    /// The canonical value form: `{"schema": <canonical node>, "default": <value>}`, then the
    /// unknown keys.
    pub fn to_value(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("schema", self.schema.to_value());
        if let Some(default) = &self.default {
            map.insert("default", default.clone());
        }
        for (key, value) in &self.extra {
            map.0.entry(key.clone()).or_insert_with(|| value.clone());
        }
        ElementProperty::Object(Box::new(map))
    }

    /// Checks one argument (or the default) against the parameter's schema, after the write
    /// coercion a schema applies (a canonical uuid string for `format: "uuid"`, an `int64`
    /// for `number`), and returns the coerced value. `query` and `parameter` name the place in
    /// the error (`Type mismatch on query 'top.limit': expected int64, got string`).
    pub fn check_value(
        &self,
        query: &str,
        parameter: &str,
        value: ElementProperty,
    ) -> Result<ElementProperty, SchemaError> {
        let value = crate::graph::schema::coerce::coerce_to_declared(value, &self.schema).map_err(
            |failure| SchemaError::CoercionFailed {
                element_type: "query",
                label: query.to_string(),
                property_name: parameter.to_string(),
                declared_type: self.schema.non_null_type_name(),
                actual_value: crate::traversal::step::barrier_steps::numeric::preview(
                    &failure.value,
                )
                .into_boxed_str(),
                reason: failure.reason,
            },
        )?;
        let ctx = crate::graph::schema::nested::NestedCtx {
            element_type: "query",
            label: query,
            additional_default: true,
            full: true,
        };
        let mut path = vec![crate::value::json_path::JsonPathSegment::Key(
            parameter.into(),
        )];
        crate::graph::schema::nested::validate_nested(&ctx, &mut path, &self.schema, &value)?;
        Ok(value)
    }

    /// Checks the schema (no `defaultFunction`: it fills a property of a created element and
    /// has no meaning for an argument) and the default (when there is one) against it.
    fn validate(&self, query: &str, parameter: &str) -> Result<(), String> {
        if let Some(at) = crate::graph::schema::defaults::default_function_in(&self.schema, "") {
            return Err(format!(
                "the schema has a defaultFunction at '{at}', which applies only to a label's \
                 properties when an element is created; give the parameter a default beside its \
                 schema instead ({{\"schema\": .., \"default\": ..}})"
            ));
        }
        let Some(default) = &self.default else {
            return Ok(());
        };
        if !storable(default) {
            return Err(
                "the default must be a data value (not a vertex, an edge or a map with \
                 non-string keys)"
                    .to_string(),
            );
        }
        self.check_value(query, parameter, default.clone())
            .map(|_| ())
            .map_err(|err| format!("the default does not satisfy the schema: {err}"))
    }
}

/// Whether `value` can be stored (no graph element, no map with a non-string key, at any
/// depth).
fn storable(value: &ElementProperty) -> bool {
    match value {
        ElementProperty::Vertex(_) | ElementProperty::Edge(_) | ElementProperty::Map(_) => false,
        ElementProperty::Array(items) => items.iter().all(storable),
        ElementProperty::Object(map) => map.values().all(storable),
        ElementProperty::Null
        | ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_)
        | ElementProperty::Number(_)
        | ElementProperty::Boolean(_)
        | ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        | ElementProperty::Vector(_) => true,
    }
}

/// Reads the parameter object of the payload / exchange form (`strict`: user input).
fn parse_params(
    value: &ElementProperty,
    strict: bool,
) -> Result<Vec<(String, QueryParam)>, DefinitionError> {
    let map = value
        .as_object()
        .ok_or_else(|| malformed("/params", "must be an object"))?;
    map.iter()
        .map(|(name, param)| Ok((name.to_string(), parse_param(name, param, strict)?)))
        .collect()
}

fn parse_param(
    parameter: &str,
    value: &ElementProperty,
    strict: bool,
) -> Result<QueryParam, DefinitionError> {
    let invalid = |reason: String| DefinitionError::InvalidParameter {
        parameter: parameter.to_string(),
        reason,
    };
    let schema_of = |node: &ElementProperty| {
        PropertySchema::from_value(node).map_err(|err| match err {
            SchemaError::InvalidSchema { path, reason, .. } => {
                invalid(format!("invalid schema at {path}: {reason}"))
            }
            other => invalid(format!("invalid schema: {other}")),
        })
    };
    if let ElementProperty::String(type_name) = value {
        let node = short_form_node(type_name).ok_or_else(|| {
            invalid(format!(
                "'{type_name}' is not a type name (integer, number, string, boolean, array, \
                 object, null, uuid)"
            ))
        })?;
        return Ok(QueryParam::new(schema_of(&node)?));
    }
    let Some(map) = value.as_object() else {
        return Err(invalid(
            "a parameter is an object {\"schema\": .., \"default\": ..} or a type name".to_string(),
        ));
    };
    let schema = schema_of(
        map.get("schema")
            .ok_or_else(|| invalid("'schema' is missing".to_string()))?,
    )?;
    let mut param = QueryParam::new(schema);
    if let Some(default) = map.get("default") {
        param = param.with_default(default.clone());
    }
    for (key, value) in map {
        if key == "schema" || key == "default" {
            continue;
        }
        if strict {
            return Err(invalid(format!(
                "unknown key '{key}' (a parameter has \"schema\" and \"default\"; a \
                 description belongs into the schema)"
            )));
        }
        param.extra.insert(key.clone(), value.clone());
    }
    Ok(param)
}

/// The schema node of a short-form parameter type name.
fn short_form_node(type_name: &str) -> Option<ElementProperty> {
    let mut node = PropertyMap::default();
    match type_name {
        "integer" | "number" | "string" | "boolean" | "array" | "object" | "null" => {
            node.insert("type", ElementProperty::from(type_name));
        }
        "uuid" => {
            node.insert("type", ElementProperty::from("string"));
            node.insert("format", ElementProperty::from("uuid"));
        }
        _ => return None,
    }
    Some(ElementProperty::Object(Box::new(node)))
}

// ---------------------------------------------------------------------------------------------
// The catalog of a TraversalGraph
// ---------------------------------------------------------------------------------------------

/// The definitions of a [`TraversalGraph`](crate::graph::TraversalGraph), by kind and name
/// (iteration: by kind code, then by name in byte order).
#[derive(Debug, Clone, Default)]
pub(crate) struct Catalog {
    by_kind: BTreeMap<DefinitionKind, BTreeMap<CompactString, Definition>>,
    len: usize,
}

impl Catalog {
    /// The definition of `kind` named `name`.
    pub(crate) fn get(&self, kind: DefinitionKind, name: &str) -> Option<&Definition> {
        self.by_kind.get(&kind)?.get(name)
    }

    /// Stores `definition`, replacing the one of the same kind and name; returns that one.
    pub(crate) fn insert(&mut self, definition: Definition) -> Option<Definition> {
        let names = self.by_kind.entry(definition.kind).or_default();
        let old = names.insert(CompactString::from(definition.name.as_str()), definition);
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    /// Removes the definition of `kind` named `name` and returns it.
    pub(crate) fn remove(&mut self, kind: DefinitionKind, name: &str) -> Option<Definition> {
        let names = self.by_kind.get_mut(&kind)?;
        let old = names.remove(name)?;
        if names.is_empty() {
            self.by_kind.remove(&kind);
        }
        self.len -= 1;
        Some(old)
    }

    /// Every definition, by kind code, then by name.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Definition> + '_ {
        self.by_kind.values().flat_map(BTreeMap::values)
    }

    /// The number of definitions.
    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn kind_value(kind: DefinitionKind) -> ElementProperty {
    match kind.name() {
        Some(name) => ElementProperty::from(name),
        None => ElementProperty::from(kind.0 as i64),
    }
}

fn malformed(at: &str, reason: impl Into<String>) -> DefinitionError {
    let reason = reason.into();
    DefinitionError::Malformed {
        reason: if at.is_empty() {
            reason
        } else {
            format!("{at}: {reason}")
        },
    }
}

fn parse_json(text: &str) -> Result<ElementProperty, DefinitionError> {
    read_json(text, MAX_JSON_DEPTH)
        .map(lift_untyped)
        .map_err(|err| malformed("", format!("not valid JSON: {err}")))
}

/// Compact JSON through the physical codec. A non-finite float (only reachable through the
/// Rust API, never from JSON) is written as `null`.
fn json_text(value: &ElementProperty) -> String {
    fn sanitize(value: PhysicalValue<'_>) -> PhysicalValue<'_> {
        match value {
            PhysicalValue::Float(f) if !f.is_finite() => PhysicalValue::Null,
            PhysicalValue::Array(items) => {
                PhysicalValue::Array(items.into_iter().map(sanitize).collect())
            }
            PhysicalValue::Object(fields) => {
                PhysicalValue::Object(fields.into_iter().map(|(k, v)| (k, sanitize(v))).collect())
            }
            other => other,
        }
    }
    let mut out = String::new();
    if write_json(&sanitize(lower(value)), &mut out).is_err() {
        out = "null".to_string();
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|[high, low]| Some(digit(*high)? << 4 | digit(*low)?))
        .collect()
}

#[cfg(feature = "serde")]
mod serde_impl {
    //! Serde of a [`Definition`]: its exchange form ([`Definition::to_value`]) with the per-value
    //! type tags of the serialized `ChangeSet` (a `uuid` default stays a `uuid`).
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::Definition;
    use crate::value::physical::{Tagged, TaggedOwned};

    impl Serialize for Definition {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            Tagged(&self.to_value()).serialize(serializer)
        }
    }

    impl<'de> Deserialize<'de> for Definition {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let TaggedOwned(value) = TaggedOwned::deserialize(deserializer)?;
            Definition::from_value(&value).map_err(serde::de::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_follow_rhai() {
        for good in ["a", "top_customers", "_x", "x1", "A_1_b"] {
            assert!(is_identifier(good), "{good}");
        }
        for bad in ["", "_", "__", "_1", "1x", "a-b", "a b", "ä", "a.b"] {
            assert!(!is_identifier(bad), "{bad}");
        }
    }

    /// What the catalog accepts as a query or parameter name, Rhai accepts as a function and a
    /// variable name (our rules are at least as strict).
    #[cfg(feature = "script")]
    #[test]
    fn accepted_names_are_valid_rhai_names() {
        let mut candidates: Vec<String> =
            RHAI_RESERVED_WORDS.iter().map(|w| w.to_string()).collect();
        candidates.extend(
            [
                "top", "_x", "x1", "Fn2", "global_x", "is_x", "a_b_c", "_", "_1", "1a",
            ]
            .map(String::from),
        );
        let engine = rhai::Engine::new_raw();
        for name in candidates {
            if identifier_problem(&name, MAX_QUERY_NAME_BYTES).is_some() {
                continue;
            }
            assert!(rhai::is_valid_function_name(&name), "{name}");
            let script = format!("fn {name}({name}) {{ {name} }}");
            assert!(engine.compile(&script).is_ok(), "{script}");
        }
    }

    #[test]
    fn hex_round_trips() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(unhex(&hex(&bytes)), Some(bytes));
        assert_eq!(unhex("0"), None);
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex("AB"), Some(vec![0xab]));
    }

    #[test]
    fn catalog_counts_and_orders() {
        let mut catalog = Catalog::default();
        let q = |name: &str| Definition::query(name, QueryDefinition::new("fn x() { 1 }"));
        assert!(catalog.insert(q("b")).is_none());
        assert!(catalog.insert(q("a")).is_none());
        assert!(catalog.insert(q("a")).is_some());
        catalog.insert(Definition::opaque(
            DefinitionKind::from_code(9),
            DefinitionFlags::NONE,
            "a",
            vec![1],
        ));
        assert_eq!(catalog.len(), 3);
        let order: Vec<(u8, &str)> = catalog
            .iter()
            .map(|d| (d.kind().code(), d.name()))
            .collect();
        assert_eq!(order, [(1, "a"), (1, "b"), (9, "a")]);
        assert!(catalog.remove(DefinitionKind::QUERY, "a").is_some());
        assert!(catalog.remove(DefinitionKind::QUERY, "a").is_none());
        assert_eq!(catalog.len(), 2);
    }
}
