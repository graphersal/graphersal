//! Compressed properties (`book/src/users_guide/compressed_properties.md`; book page "Compressed
//! Properties"): rules that keep large strings LZ4-compressed in memory, or long repeated strings
//! stored once (codec `dedup`).
//!
//! A rule is a catalog definition of kind [`DefinitionKind::COMPRESSION`]
//! ([`CompressionDefinition`]): element kind + label + a jpath of names. A string stored at a
//! rule's path is kept as [`ElementProperty::Compressed`] when it is at least
//! [`CompressionDefinition::min_bytes`] long and its compressed form is at most 90 % of the plain
//! size; every other stored value is plain (**the invariant**, D6). Under a `dedup` rule a string
//! at the path is kept as [`ElementProperty::Deduped`] when it is at least `min_bytes` and longer
//! than the inline capacity of a `CompactString` (24 bytes on 64-bit targets: a shorter string
//! owns no heap to share), and it points to the ONE copy of its text in the rule's table. The
//! graph keeps the rules' in-memory state here (`CompressionRules`: the parsed paths, the
//! dictionaries and the dedup tables), derived from the catalog like an index:
//!
//! - the write path normalizes the touched top-level key (D7), a label change the vertex;
//! - a define, redefine or drop runs the **explicit pass** over the rule's `(element, label)`
//!   (D8); `recompress` re-runs it and rebuilds the dictionary;
//! - a rollback normalizes what it restores and re-runs the pass of an undone rule change (D10);
//! - a load runs one pass over every rule after the elements and the catalog are in place (D11).
//!
//! A dedup table entry that no element references any more (its value was overwritten or its
//! element dropped) is swept lazily: when the table has doubled since its last sweep, after a
//! rollback, by `recompress` and by a load (each table holds at most about twice its live
//! entries).
//!
//! The pass changes no logical data: it records nothing in the undo log and emits no mutation
//! (a unit that only recompresses commits nothing).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use compact_str::CompactString;

use crate::error::{DefinitionError, GraphError};
use crate::graph::TraversalGraph;
use crate::graph::definition::{Definition, DefinitionBody, DefinitionKind};
use crate::graph::schema::ElementKind;
use crate::value::compressed::{CompressedText, CompressionDictionary, MAX_DICTIONARY_BYTES};
use crate::value::deduped::{DEDUP_ENTRY_OVERHEAD, DedupedText};
use crate::value::json_path::JsonPathSegment;
use crate::value::{ElementProperty, JsonPath, PropertyMap};

/// The default [`CompressionDefinition::min_bytes`] of an `lz4` rule.
pub const DEFAULT_MIN_BYTES: u32 = 256;
/// The default [`CompressionDefinition::min_bytes`] of a `dedup` rule: one byte more than the
/// inline capacity of a string on 64-bit targets (24 bytes), the shortest string that owns heap
/// to share.
pub const DEFAULT_DEDUP_MIN_BYTES: u32 = 25;
/// A string of at most this many bytes is stored inline (no heap): a `dedup` rule never shares
/// it, whatever its `min_bytes` (24 on 64-bit targets).
pub(crate) const INLINE_TEXT_BYTES: usize = std::mem::size_of::<CompactString>();
/// The largest [`CompressionDefinition::min_bytes`]: the default `traversal.max_string_bytes`
/// (16 MiB); a longer threshold could never be reached by a string a step builds.
pub const MAX_MIN_BYTES: u32 = 16 * 1024 * 1024;
/// The most segments a rule's path may have.
pub const MAX_PATH_SEGMENTS: usize = 32;
/// The longest label of a rule, in bytes.
pub const MAX_LABEL_BYTES: usize = 1024;
/// How many bytes of one value the dictionary sample takes at most.
const SAMPLE_BYTES_PER_VALUE: usize = 1024;

/// The codec of a compression rule: `lz4` (large text, compressed per value) or `dedup` (long
/// repeated values, stored once per distinct value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Codec {
    /// LZ4 block compression (`lz4_flex`), optionally with a dictionary.
    #[default]
    Lz4,
    /// Deduplication: every distinct value at the rule's path is stored once in the rule's
    /// table and every element keeps a reference to it (book page "Compressed Properties",
    /// section "Deduplicating repeated values"). No dictionary.
    Dedup,
}

impl Codec {
    /// The codec's name (`"lz4"`, `"dedup"`).
    pub fn name(self) -> &'static str {
        match self {
            Codec::Lz4 => "lz4",
            Codec::Dedup => "dedup",
        }
    }

    /// The codec with this [name](Self::name).
    pub fn from_name(name: &str) -> Option<Codec> {
        match name {
            "lz4" => Some(Codec::Lz4),
            "dedup" => Some(Codec::Dedup),
            _ => None,
        }
    }

    /// The `min_bytes` of a rule of this codec that does not set one: [`DEFAULT_MIN_BYTES`]
    /// for `lz4`, [`DEFAULT_DEDUP_MIN_BYTES`] for `dedup`.
    pub fn default_min_bytes(self) -> u32 {
        match self {
            Codec::Lz4 => DEFAULT_MIN_BYTES,
            Codec::Dedup => DEFAULT_DEDUP_MIN_BYTES,
        }
    }

    /// Whether a rule of this codec can build a dictionary (`lz4` only); it is also the
    /// `dictionary` of a rule that does not set one.
    pub fn supports_dictionary(self) -> bool {
        match self {
            Codec::Lz4 => true,
            Codec::Dedup => false,
        }
    }
}

/// A compression rule (catalog kind [`DefinitionKind::COMPRESSION`]): strings of `element`s
/// labelled `label` at `path` are kept compressed in memory.
///
/// ```
/// use graphersal::catalog::CompressionDefinition;
/// use graphersal::storage::ElementKind;
///
/// let rule = CompressionDefinition::new(ElementKind::Vertex, "customer", "details.career.cv")
///     .unwrap()
///     .with_min_bytes(512)
///     .with_dictionary(false);
/// assert_eq!(rule.path_text(), "$.details.career.cv");
/// assert_eq!(rule.min_bytes(), 512);
/// ```
///
/// A `dedup` rule stores each distinct value once; its defaults are `min_bytes` 25 and no
/// dictionary:
///
/// ```
/// use graphersal::catalog::{Codec, CompressionDefinition};
/// use graphersal::storage::ElementKind;
///
/// let rule = CompressionDefinition::new(ElementKind::Vertex, "symbol", "path")
///     .unwrap()
///     .with_codec(Codec::Dedup);
/// assert_eq!(rule.min_bytes(), 25);
/// assert!(!rule.dictionary());
/// ```
///
/// Equality compares the effective parameters: a threshold or dictionary flag left at the
/// codec's default equals the same value given explicitly.
///
/// A rule read from a payload whose `codec`, `element` or `path` this version does not know
/// (a newer Graphersal wrote it) is kept, never applied: its values stay plain, it is listed
/// with [`Self::unserved_reason`], and its payload is written back exactly as read (format
/// spec 6.3, 16). Front ends never create one: they take only the known codec names.
#[derive(Debug, Clone)]
pub struct CompressionDefinition {
    element: ElementKind,
    label: String,
    path: JsonPath,
    codec: Codec,
    /// `None`: the codec's default ([`Codec::default_min_bytes`]).
    min_bytes: Option<u32>,
    /// `None`: the codec's default ([`Codec::supports_dictionary`]).
    dictionary: Option<bool>,
    extra: PropertyMap,
    /// The parts this version does not know and the payload as read (`None`: a rule this
    /// version applies).
    foreign: Option<Box<ForeignRule>>,
}

/// What a newer version wrote into a compression rule that this version does not know.
#[derive(Debug, Clone, PartialEq)]
struct ForeignRule {
    /// The unknown codec name, as read.
    codec: Option<String>,
    /// The unknown element kind name, as read.
    element: Option<String>,
    /// The path text this version cannot parse, as read.
    path: Option<String>,
    /// The whole payload as read, written back unchanged.
    payload: PropertyMap,
}

impl PartialEq for CompressionDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.element == other.element
            && self.label == other.label
            && self.path == other.path
            && self.codec == other.codec
            && self.min_bytes() == other.min_bytes()
            && self.dictionary() == other.dictionary()
            && self.extra == other.extra
            && self.foreign == other.foreign
    }
}

impl CompressionDefinition {
    /// A rule for `element`s labelled `label` at `path` (a jpath of names: `"cv"`,
    /// `"details.career.cv"`, `"$.a['b c']"`), with the defaults `lz4`, `min_bytes` 256 and a
    /// dictionary (a `dedup` rule, [`Self::with_codec`], defaults to `min_bytes` 25 and no
    /// dictionary). A path that does not parse is an error; an index segment or an empty path is
    /// rejected by [`Definition::validate`] (and by this constructor).
    pub fn new(
        element: ElementKind,
        label: impl Into<String>,
        path: &str,
    ) -> Result<CompressionDefinition, DefinitionError> {
        let path: JsonPath =
            path.parse()
                .map_err(|err: crate::TraverserError| DefinitionError::InvalidField {
                    name: String::new(),
                    field: "path".to_string(),
                    reason: err.to_string(),
                })?;
        let rule = Self::with_path(element, label, path);
        if let Some(reason) = path_problem(&rule.path) {
            return Err(DefinitionError::InvalidField {
                name: String::new(),
                field: "path".to_string(),
                reason,
            });
        }
        Ok(rule)
    }

    /// A rule with an already parsed path (checked by [`Definition::validate`]).
    pub fn with_path(element: ElementKind, label: impl Into<String>, path: JsonPath) -> Self {
        CompressionDefinition {
            element,
            label: label.into(),
            path,
            codec: Codec::Lz4,
            min_bytes: None,
            dictionary: None,
            extra: PropertyMap::default(),
            foreign: None,
        }
    }

    /// The same rule with this threshold: strings shorter than `min_bytes` (UTF-8 bytes) stay
    /// plain. On a rule of a newer version ([`Self::unserved_reason`]) it changes nothing:
    /// such a rule is kept as read.
    pub fn with_min_bytes(mut self, min_bytes: u32) -> Self {
        if self.foreign.is_none() {
            self.min_bytes = Some(min_bytes);
        }
        self
    }

    /// The same rule with or without a dictionary (`lz4` only: a `dedup` rule with a
    /// dictionary is refused by [`Definition::validate`]). On a rule of a newer version it
    /// changes nothing.
    pub fn with_dictionary(mut self, dictionary: bool) -> Self {
        if self.foreign.is_none() {
            self.dictionary = Some(dictionary);
        }
        self
    }

    /// The same rule with this codec (a threshold and a dictionary flag not set explicitly
    /// follow the codec's defaults). On a rule of a newer version it changes nothing.
    pub fn with_codec(mut self, codec: Codec) -> Self {
        if self.foreign.is_none() {
            self.codec = codec;
        }
        self
    }

    /// Why this version keeps the rule without applying it, `None` when it applies it: its
    /// `codec`, `element` or `path` was written by a newer Graphersal (`"codec 'zstd' is not
    /// known to this version ..."`). Its values stay plain; listings show it next to
    /// `served: false`.
    ///
    /// ```
    /// use graphersal::catalog::CompressionDefinition;
    /// use graphersal::prelude::PropertyMap;
    ///
    /// let mut payload = PropertyMap::default();
    /// payload.insert("label", "doc".into());
    /// payload.insert("path", "body".into());
    /// payload.insert("codec", "zstd".into());
    /// let rule = CompressionDefinition::from_payload(&payload).unwrap();
    /// assert!(rule.unserved_reason().unwrap().contains("'zstd'"));
    /// assert_eq!(rule.codec_name(), "zstd");
    /// assert_eq!(rule.to_payload(), payload);
    /// ```
    pub fn unserved_reason(&self) -> Option<String> {
        let foreign = self.foreign.as_ref()?;
        let what = if let Some(codec) = &foreign.codec {
            format!("codec '{codec}'")
        } else if let Some(element) = &foreign.element {
            format!("element kind '{element}'")
        } else if let Some(path) = &foreign.path {
            format!("path '{path}'")
        } else {
            "the rule".to_string()
        };
        Some(format!(
            "{what} is not known to this version (a newer Graphersal wrote it); the rule is \
             kept and not applied, its values stay plain"
        ))
    }

    /// Whether this version applies the rule (its codec, element kind and path are known).
    pub(crate) fn is_served(&self) -> bool {
        self.foreign.is_none()
    }

    /// The codec's name: [`Codec::name`], or the unknown name as read.
    pub fn codec_name(&self) -> &str {
        match self.foreign.as_ref().and_then(|f| f.codec.as_deref()) {
            Some(name) => name,
            None => self.codec.name(),
        }
    }

    /// The element kind's name (`"vertex"`, `"edge"`), or the unknown name as read.
    pub fn element_name(&self) -> &str {
        match self.foreign.as_ref().and_then(|f| f.element.as_deref()) {
            Some(name) => name,
            None => self.element.as_str(),
        }
    }

    /// The element kind the rule applies to.
    pub fn element(&self) -> ElementKind {
        self.element
    }

    /// The label the rule applies to (it need not exist yet).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The path of names inside the element's properties.
    pub fn path(&self) -> &JsonPath {
        &self.path
    }

    /// The canonical text of the path (`$.details.career.cv`); the text as read for a path
    /// this version cannot parse.
    pub fn path_text(&self) -> String {
        match self.foreign.as_ref().and_then(|f| f.path.as_deref()) {
            Some(text) => text.to_string(),
            None => self.path.to_string(),
        }
    }

    /// The codec.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Strings shorter than this many UTF-8 bytes stay plain (the codec's default when not
    /// set: 256 for `lz4`, 25 for `dedup`).
    pub fn min_bytes(&self) -> u32 {
        self.min_bytes
            .unwrap_or_else(|| self.codec.default_min_bytes())
    }

    /// Whether the explicit pass builds a dictionary from the rule's values (the codec's
    /// default when not set: true for `lz4`, false for `dedup`).
    pub fn dictionary(&self) -> bool {
        self.dictionary
            .unwrap_or_else(|| self.codec.supports_dictionary())
    }

    /// Payload keys this version does not know, kept and written back.
    pub fn extra(&self) -> &PropertyMap {
        &self.extra
    }

    /// The path segments, all names (checked by [`Self::validate`]).
    fn names(&self) -> Vec<CompactString> {
        self.path
            .segments()
            .iter()
            .filter_map(|segment| match segment {
                JsonPathSegment::Key(name) => Some(name.clone()),
                JsonPathSegment::Index(_) => None,
            })
            .collect()
    }

    /// The payload property map (format spec 6.3), in this key order: `element` (`"vertex"` or
    /// `"edge"`), `label`, `path` (canonical jpath text), `codec` (`"lz4"` or `"dedup"`),
    /// `min_bytes` (int64), `dictionary` (boolean), then the unknown keys. The effective
    /// threshold and dictionary flag are always written. A rule of a newer version
    /// ([`Self::unserved_reason`]) gives its payload back exactly as read.
    pub fn to_payload(&self) -> PropertyMap {
        if let Some(foreign) = &self.foreign {
            return foreign.payload.clone();
        }
        let mut map = PropertyMap::default();
        map.insert("element", self.element.as_str().into());
        map.insert("label", self.label.as_str().into());
        map.insert("path", self.path_text().into());
        map.insert("codec", self.codec.name().into());
        map.insert("min_bytes", ElementProperty::from(self.min_bytes() as i64));
        map.insert("dictionary", ElementProperty::Boolean(self.dictionary()));
        for (key, value) in &self.extra {
            map.insert_if_absent(key, || value.clone());
        }
        map
    }

    /// Reads a payload map (the inverse of [`Self::to_payload`]; `skip` lists envelope keys of
    /// the exchange form). Missing optional keys take their defaults; a known key of the wrong
    /// type is an error; unknown keys are kept. A `codec` or `element` name this version does
    /// not know, or a `path` it cannot parse, is never an error: the rule is kept as read and
    /// not applied ([`Self::unserved_reason`]). Not validated.
    pub fn from_payload(payload: &PropertyMap) -> Result<CompressionDefinition, DefinitionError> {
        Self::read_fields(payload, &[])
    }

    pub(crate) fn read_fields(
        map: &PropertyMap,
        skip: &[&str],
    ) -> Result<CompressionDefinition, DefinitionError> {
        let malformed = |key: &str, reason: &str| DefinitionError::Malformed {
            reason: format!("/{key}: {reason}"),
        };
        let text = |key: &str| -> Result<Option<String>, DefinitionError> {
            match map.get(key) {
                None => Ok(None),
                Some(value) => value
                    .text()
                    .map(|text| Some(text.into_owned()))
                    .ok_or_else(|| malformed(key, "must be a string")),
            }
        };
        let mut foreign = ForeignRule {
            codec: None,
            element: None,
            path: None,
            payload: PropertyMap::default(),
        };
        let element = match text("element")?.as_deref() {
            None | Some("vertex") => ElementKind::Vertex,
            Some("edge") => ElementKind::Edge,
            Some(other) => {
                foreign.element = Some(other.to_string());
                ElementKind::Vertex
            }
        };
        let label = text("label")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'label' is missing".to_string(),
        })?;
        let path_text = text("path")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'path' is missing".to_string(),
        })?;
        let path: JsonPath = match path_text.parse() {
            Ok(path) => path,
            Err(_) => {
                foreign.path = Some(path_text);
                JsonPath(Arc::new(Vec::new()))
            }
        };
        let mut rule = CompressionDefinition::with_path(element, label, path);
        if let Some(codec) = text("codec")? {
            match Codec::from_name(&codec) {
                Some(known) => rule.codec = known,
                None => foreign.codec = Some(codec),
            }
        }
        if let Some(value) = map.get("min_bytes") {
            rule.min_bytes = Some(
                value
                    .as_i64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| malformed("min_bytes", "must be a number from 0 to 2^32-1"))?,
            );
        }
        if let Some(value) = map.get("dictionary") {
            rule.dictionary = Some(
                value
                    .as_boolean()
                    .ok_or_else(|| malformed("dictionary", "must be a boolean"))?,
            );
        }
        const KNOWN: [&str; 6] = [
            "element",
            "label",
            "path",
            "codec",
            "min_bytes",
            "dictionary",
        ];
        for (key, value) in map {
            if !KNOWN.contains(&key.as_str()) && !skip.contains(&key.as_str()) {
                rule.extra.insert(key.clone(), value.to_logical());
            }
        }
        if foreign.codec.is_some() || foreign.element.is_some() || foreign.path.is_some() {
            for (key, value) in map {
                if !skip.contains(&key.as_str()) {
                    foreign.payload.insert(key.clone(), value.to_logical());
                }
            }
            rule.foreign = Some(Box::new(foreign));
        }
        Ok(rule)
    }

    /// Writes the exchange-form fields (the payload keys) into `map`.
    pub(crate) fn write_fields(&self, map: &mut PropertyMap) {
        for (key, value) in self.to_payload() {
            map.insert(key, value);
        }
    }

    /// The rules of [`Definition::validate`] for a rule named `name`.
    pub(crate) fn validate(&self, name: &str) -> Result<(), DefinitionError> {
        let field = |field: &str, reason: String| DefinitionError::InvalidField {
            name: name.to_string(),
            field: field.to_string(),
            reason,
        };
        if self.label.is_empty() {
            return Err(field("label", "it is empty".to_string()));
        }
        if self.label.len() > MAX_LABEL_BYTES {
            return Err(field(
                "label",
                format!("it is longer than {MAX_LABEL_BYTES} bytes"),
            ));
        }
        if self.label.contains("::") {
            return Err(field(
                "label",
                "a label cannot contain '::' (the multi-label separator)".to_string(),
            ));
        }
        // A rule of a newer version is kept as read: its parameters follow that version's
        // rules, which this one does not know.
        if self.foreign.is_some() {
            return self.check_reserved_keys(name);
        }
        if let Some(reason) = path_problem(&self.path) {
            return Err(field("path", reason));
        }
        let min_bytes = self.min_bytes();
        if min_bytes == 0 || min_bytes > MAX_MIN_BYTES {
            return Err(field(
                "min_bytes",
                format!(
                    "{min_bytes} is outside 1 to {MAX_MIN_BYTES} (the default traversal.max_string_bytes)"
                ),
            ));
        }
        if self.dictionary() && !self.codec.supports_dictionary() {
            return Err(field(
                "dictionary",
                format!(
                    "a {} rule has no dictionary (it stores each distinct value once); leave \
                     dictionary out or set it to false",
                    self.codec.name()
                ),
            ));
        }
        self.check_reserved_keys(name)
    }

    /// No unknown payload key is a reserved one.
    fn check_reserved_keys(&self, name: &str) -> Result<(), DefinitionError> {
        match self
            .extra
            .keys()
            .find(|key| crate::graph::definition::RESERVED_PAYLOAD_KEYS.contains(&key.as_str()))
        {
            Some(key) => Err(DefinitionError::InvalidField {
                name: name.to_string(),
                field: "payload".to_string(),
                reason: format!("the payload key '{key}' is reserved"),
            }),
            None => Ok(()),
        }
    }
}

/// Why `path` cannot be a rule's path (`None`: it can): it must have 1 to
/// [`MAX_PATH_SEGMENTS`] segments, all names.
pub(crate) fn path_problem(path: &JsonPath) -> Option<String> {
    let segments = path.segments();
    if segments.is_empty() {
        return Some("the path is empty; name the property, for example \"cv\"".to_string());
    }
    if segments.len() > MAX_PATH_SEGMENTS {
        return Some(format!(
            "the path has {} segments, at most {MAX_PATH_SEGMENTS} are allowed",
            segments.len()
        ));
    }
    if segments
        .iter()
        .any(|segment| matches!(segment, JsonPathSegment::Index(_)))
    {
        return Some(format!(
            "'{path}' has an index segment; a rule path names object keys only (arrays of \
             strings are not compressed in this version)"
        ));
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The in-memory state of the rules
// ---------------------------------------------------------------------------------------------

/// The table of a `dedup` rule: the one shared copy of every distinct value the rule stores.
/// An entry is live while an element (or the undo log, or a copy a caller holds) references
/// it; one only the table holds is swept lazily (module docs).
#[derive(Debug, Default)]
pub(crate) struct DedupTable {
    entries: HashSet<Arc<str>>,
    /// The size at which the next insertion sweeps first.
    sweep_at: usize,
}

/// The smallest size at which [`DedupTable`] sweeps before an insertion.
const DEDUP_SWEEP_MIN: usize = 256;

impl DedupTable {
    /// The shared copy of `text`, made when the table has none.
    fn intern(&mut self, text: &str) -> Arc<str> {
        if let Some(shared) = self.entries.get(text) {
            return shared.clone();
        }
        self.insert(Arc::from(text))
    }

    /// The table's copy of the text `shared` holds: `shared` itself when the table has none
    /// (it becomes the table's copy), `None` when it already IS the table's copy.
    fn adopt(&mut self, shared: &Arc<str>) -> Option<Arc<str>> {
        match self.entries.get(&**shared) {
            Some(own) if Arc::ptr_eq(own, shared) => None,
            Some(own) => Some(own.clone()),
            None => {
                self.insert(shared.clone());
                None
            }
        }
    }

    fn insert(&mut self, shared: Arc<str>) -> Arc<str> {
        if self.entries.len() >= self.sweep_at {
            self.sweep();
        }
        self.entries.insert(shared.clone());
        shared
    }

    /// Drops every entry only the table holds.
    pub(crate) fn sweep(&mut self) {
        self.entries.retain(|shared| Arc::strong_count(shared) > 1);
        self.sweep_at = (2 * self.entries.len()).max(DEDUP_SWEEP_MIN);
    }

    /// Whether `shared` is the table's copy of its text.
    fn holds(&self, shared: &Arc<str>) -> bool {
        self.entries
            .get(&**shared)
            .is_some_and(|own| Arc::ptr_eq(own, shared))
    }

    /// How many entries the table holds (live or not yet swept).
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The heap bytes of the table: every shared text once (its bytes and the two reference
    /// counts of its allocation) and the hash set's slots (a fat pointer and a control byte
    /// per bucket).
    pub(crate) fn heap_bytes(&self) -> u64 {
        let counts = 2 * std::mem::size_of::<usize>();
        let texts: usize = self.entries.iter().map(|text| text.len() + counts).sum();
        let slots = self.entries.capacity() * (std::mem::size_of::<Arc<str>>() + 1);
        (texts + slots) as u64
    }
}

/// One active rule.
#[derive(Debug, Clone)]
pub(crate) struct Rule {
    name: CompactString,
    element: ElementKind,
    label: CompactString,
    path: Vec<CompactString>,
    codec: Codec,
    min_bytes: usize,
    use_dictionary: bool,
    dictionary: Option<Arc<CompressionDictionary>>,
    /// The table of a `dedup` rule (`None` for `lz4`). Behind a mutex: the write path
    /// normalizes under a shared borrow of the rules.
    dedup: Option<Arc<parking_lot::Mutex<DedupTable>>>,
}

/// The rules of a graph, by element kind and label (derived from the catalog).
#[derive(Debug, Clone, Default)]
pub(crate) struct CompressionRules {
    vertex: HashMap<CompactString, Vec<Rule>>,
    edge: HashMap<CompactString, Vec<Rule>>,
}

impl CompressionRules {
    /// Whether no rule exists at all.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.vertex.is_empty() && self.edge.is_empty()
    }

    fn by_kind(&self, element: ElementKind) -> &HashMap<CompactString, Vec<Rule>> {
        match element {
            ElementKind::Vertex => &self.vertex,
            ElementKind::Edge => &self.edge,
        }
    }

    fn by_kind_mut(&mut self, element: ElementKind) -> &mut HashMap<CompactString, Vec<Rule>> {
        match element {
            ElementKind::Vertex => &mut self.vertex,
            ElementKind::Edge => &mut self.edge,
        }
    }

    /// The rules of an element with these labels for the top-level key `key`, as (path below
    /// `key`, rule); the first label (label order) that declares a path wins it.
    fn for_key<'a>(
        &'a self,
        element: ElementKind,
        labels: &[&str],
        key: &str,
    ) -> Vec<(&'a [CompactString], &'a Rule)> {
        let mut out: Vec<(&[CompactString], &Rule)> = Vec::new();
        let by_kind = self.by_kind(element);
        for label in labels {
            let Some(rules) = by_kind.get(*label) else {
                continue;
            };
            for rule in rules {
                if rule.path.first().is_some_and(|first| first == key)
                    && !out.iter().any(|(rest, _)| *rest == &rule.path[1..])
                {
                    out.push((&rule.path[1..], rule));
                }
            }
        }
        out
    }

    /// Whether any rule applies to an element with these labels.
    fn applies(&self, element: ElementKind, labels: &[&str]) -> bool {
        let by_kind = self.by_kind(element);
        !by_kind.is_empty() && labels.iter().any(|label| by_kind.contains_key(*label))
    }

    /// Normalizes every top-level key of `properties` (see [`normalize_value`]).
    pub(crate) fn normalize_element(
        &self,
        element: ElementKind,
        labels: &[&str],
        properties: &mut PropertyMap,
        force: bool,
    ) {
        let applies = self.applies(element, labels);
        for (key, value) in properties.iter_mut() {
            if applies {
                let rules = self.for_key(element, labels, key);
                normalize_value(value, &rules, force);
            } else {
                value.make_logical();
            }
        }
    }

    /// Normalizes the top-level key `key` of `properties`.
    pub(crate) fn normalize_key(
        &self,
        element: ElementKind,
        labels: &[&str],
        properties: &mut PropertyMap,
        key: &str,
    ) {
        let Some(value) = properties.get_mut(key) else {
            return;
        };
        if self.applies(element, labels) {
            let rules = self.for_key(element, labels, key);
            normalize_value(value, &rules, false);
        } else {
            value.make_logical();
        }
    }

    /// Normalizes a value that is about to be stored under `key`.
    pub(crate) fn normalize_incoming(
        &self,
        element: ElementKind,
        labels: &[&str],
        key: &str,
        value: &mut ElementProperty,
    ) {
        if self.applies(element, labels) {
            let rules = self.for_key(element, labels, key);
            normalize_value(value, &rules, false);
        } else {
            value.make_logical();
        }
    }

    /// Normalizes a value about to be stored under the top-level vertex key `key` (one
    /// `is_empty` check without rules).
    pub(crate) fn prepare_vertex_value(
        &self,
        labels: &[CompactString],
        key: &str,
        value: &mut ElementProperty,
    ) {
        if self.is_empty() && !value.holds_compressed() {
            return;
        }
        let labels: Vec<&str> = labels.iter().map(CompactString::as_str).collect();
        self.normalize_incoming(ElementKind::Vertex, &labels, key, value);
    }

    /// Normalizes a value about to be stored under the top-level edge key `key`.
    pub(crate) fn prepare_edge_value(
        &self,
        label: Option<&str>,
        key: &str,
        value: &mut ElementProperty,
    ) {
        if self.is_empty() && !value.holds_compressed() {
            return;
        }
        let labels: Vec<&str> = label.into_iter().collect();
        self.normalize_incoming(ElementKind::Edge, &labels, key, value);
    }

    /// The rules derived from `definitions`, keeping the dictionaries of `previous` for rules
    /// whose name and parameters did not change.
    fn from_catalog<'a>(
        definitions: impl Iterator<Item = &'a Definition>,
        previous: &CompressionRules,
    ) -> CompressionRules {
        let mut rules = CompressionRules::default();
        for definition in definitions {
            let Some(rule) = definition.as_compression().filter(|rule| rule.is_served()) else {
                continue;
            };
            let mut state = Rule {
                name: CompactString::from(definition.name()),
                element: rule.element(),
                label: CompactString::from(rule.label()),
                path: rule.names(),
                codec: rule.codec(),
                min_bytes: rule.min_bytes() as usize,
                use_dictionary: rule.dictionary(),
                dictionary: None,
                dedup: None,
            };
            let kept = previous
                .find(&state.name)
                .filter(|old| old.same_parameters(&state));
            if state.use_dictionary {
                state.dictionary = kept.and_then(|old| old.dictionary.clone());
            }
            match state.codec {
                Codec::Dedup => {
                    state.dedup = Some(kept.and_then(|old| old.dedup.clone()).unwrap_or_default());
                }
                Codec::Lz4 => {}
            }
            rules
                .by_kind_mut(state.element)
                .entry(state.label.clone())
                .or_default()
                .push(state);
        }
        rules
    }

    fn find(&self, name: &str) -> Option<&Rule> {
        self.vertex
            .values()
            .chain(self.edge.values())
            .flatten()
            .find(|rule| rule.name == name)
    }

    fn find_mut(&mut self, name: &str) -> Option<&mut Rule> {
        self.vertex
            .values_mut()
            .chain(self.edge.values_mut())
            .flatten()
            .find(|rule| rule.name == name)
    }

    /// Every rule.
    fn iter(&self) -> impl Iterator<Item = &Rule> {
        self.vertex.values().chain(self.edge.values()).flatten()
    }

    /// Sweeps every dedup table (after a rollback: entries only the table holds go).
    pub(crate) fn sweep_dedup(&self) {
        for table in self.iter().filter_map(|rule| rule.dedup.as_ref()) {
            table.lock().sweep();
        }
    }

    /// The summed heap bytes of the dedup tables (each table once).
    pub(crate) fn dedup_table_bytes(&self) -> u64 {
        self.iter()
            .filter_map(|rule| rule.dedup.as_ref())
            .map(|table| table.lock().heap_bytes())
            .sum()
    }

    /// The summed size of the distinct dictionaries.
    pub(crate) fn dictionary_bytes(&self) -> u64 {
        let mut seen: Vec<*const CompressionDictionary> = Vec::new();
        let mut total = 0u64;
        for dictionary in self.iter().filter_map(|rule| rule.dictionary.as_ref()) {
            let pointer = Arc::as_ptr(dictionary);
            if !seen.contains(&pointer) {
                seen.push(pointer);
                total += dictionary.len() as u64;
            }
        }
        total
    }
}

impl Rule {
    fn same_parameters(&self, other: &Rule) -> bool {
        self.element == other.element
            && self.label == other.label
            && self.path == other.path
            && self.codec == other.codec
            && self.min_bytes == other.min_bytes
            && self.use_dictionary == other.use_dictionary
    }

    /// The compressed form of `text` under this (`lz4`) rule, `None` when it stays plain.
    fn encode(&self, text: &str) -> Option<CompressedText> {
        if text.len() < self.min_bytes {
            return None;
        }
        CompressedText::compress(text, self.dictionary.as_ref())
    }

    /// Whether a `dedup` rule shares a string of `len` bytes.
    fn dedup_qualifies(&self, len: usize) -> bool {
        dedup_qualifies(len, self.min_bytes)
    }

    /// The shared form of `text` under this `dedup` rule, `None` when it stays plain.
    fn share(&self, text: &str) -> Option<ElementProperty> {
        let table = self.dedup.as_ref()?;
        self.dedup_qualifies(text.len())
            .then(|| ElementProperty::Deduped(DedupedText::new(table.lock().intern(text))))
    }

    fn same_dictionary(&self, value: &CompressedText) -> bool {
        match (&self.dictionary, value.dictionary()) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Brings `value` (stored under a top-level key whose rules below that key are `rules`) into the
/// invariant: a string at a rule leaf compressed when it qualifies, every other compressed value
/// decompressed. `force` re-encodes compressed values made with another dictionary.
fn normalize_value(value: &mut ElementProperty, rules: &[(&[CompactString], &Rule)], force: bool) {
    if rules.is_empty() {
        value.make_logical();
        return;
    }
    if let Some((_, rule)) = rules.iter().find(|(rest, _)| rest.is_empty()) {
        match rule.codec {
            Codec::Lz4 => normalize_lz4(value, rule, force),
            Codec::Dedup => normalize_dedup(value, rule),
        }
        return;
    }
    match value {
        ElementProperty::Object(map) => {
            for (key, child) in map.iter_mut() {
                let below: Vec<(&[CompactString], &Rule)> = rules
                    .iter()
                    .filter(|(rest, _)| rest.first().is_some_and(|first| first == key))
                    .map(|(rest, rule)| (&rest[1..], *rule))
                    .collect();
                normalize_value(child, &below, force);
            }
        }
        _ => value.make_logical(),
    }
}

/// [`normalize_value`] at the leaf of an `lz4` rule.
fn normalize_lz4(value: &mut ElementProperty, rule: &Rule, force: bool) {
    match value {
        ElementProperty::String(text) => {
            if let Some(compressed) = rule.encode(text) {
                *value = ElementProperty::Compressed(Box::new(compressed));
            }
        }
        ElementProperty::Compressed(compressed) => {
            if compressed.plain_len() < rule.min_bytes
                || (force && !rule.same_dictionary(compressed))
            {
                let text = compressed.text();
                *value = match rule.encode(&text) {
                    Some(again) => ElementProperty::Compressed(Box::new(again)),
                    None => ElementProperty::String(text),
                };
            }
        }
        // Shared by a former dedup rule: compressed or plain now.
        ElementProperty::Deduped(shared) => {
            *value = match rule.encode(shared.as_str()) {
                Some(compressed) => ElementProperty::Compressed(Box::new(compressed)),
                None => ElementProperty::String(shared.as_str().into()),
            };
        }
        // A non-string at a rule path is left as it is (nothing below it is a rule leaf).
        _ => value.make_logical(),
    }
}

/// [`normalize_value`] at the leaf of a `dedup` rule: a qualifying string becomes (or stays) a
/// reference to the table's copy, anything else at the leaf is plain.
fn normalize_dedup(value: &mut ElementProperty, rule: &Rule) {
    match value {
        ElementProperty::String(text) => {
            if let Some(shared) = rule.share(text) {
                *value = shared;
            }
        }
        ElementProperty::Deduped(shared) => {
            if !rule.dedup_qualifies(shared.len()) {
                *value = ElementProperty::String(shared.as_str().into());
            } else if let Some(table) = rule.dedup.as_ref() {
                // A value restored from the undo log, or made under the rule's former table,
                // points to the current table's copy.
                if let Some(own) = table.lock().adopt(shared.shared()) {
                    *value = ElementProperty::Deduped(DedupedText::new(own));
                }
            }
        }
        // Compressed by a former lz4 rule: shared or plain now.
        ElementProperty::Compressed(compressed) => {
            let text = compressed.text();
            *value = rule.share(&text).unwrap_or(ElementProperty::String(text));
        }
        _ => value.make_logical(),
    }
}

/// Whether a `dedup` rule with threshold `min_bytes` shares a string of `len` bytes: at least
/// `min_bytes` and longer than the inline capacity (a shorter string owns no heap). The ONE
/// qualification of the write path, the invariant check and the estimate.
pub(crate) fn dedup_qualifies(len: usize, min_bytes: usize) -> bool {
    len >= min_bytes && len > INLINE_TEXT_BYTES
}

/// The value at `path` inside `properties`, when every step is an object key.
pub(crate) fn value_at<'a>(
    properties: &'a PropertyMap,
    path: &[CompactString],
) -> Option<&'a ElementProperty> {
    let (first, rest) = path.split_first()?;
    let mut current = properties.get(first.as_str())?;
    for key in rest {
        current = match current {
            ElementProperty::Object(map) => map.get(key.as_str())?,
            _ => return None,
        };
    }
    Some(current)
}

/// The dictionary of a rule over `elements` (in id order, [`TraversalGraph::compression_elements`]):
/// the text at `path` of each, at most 1 KiB of each, up to 64 KiB. The ONE sampling code of a
/// rule's dictionary and of the estimate's trial dictionary (`compression_estimate.rs`), so an
/// estimate measures the dictionary the rule would get.
pub(crate) fn dictionary_from(
    elements: &[(&str, &PropertyMap)],
    path: &[CompactString],
) -> Option<Arc<CompressionDictionary>> {
    let mut sample: Vec<u8> = Vec::new();
    for (_, properties) in elements {
        if sample.len() >= MAX_DICTIONARY_BYTES {
            break;
        }
        if let Some(value) = value_at(properties, path) {
            value.with_text(|text| {
                let mut end = text.len().min(SAMPLE_BYTES_PER_VALUE);
                end = end.min(MAX_DICTIONARY_BYTES - sample.len());
                sample.extend_from_slice(&text.as_bytes()[..end]);
            });
        }
    }
    CompressionDictionary::new(sample)
}

/// What one rule holds right now ([`TraversalGraph::compression_stats`]).
///
/// For a `dedup` rule `compressed_values` counts the references (one per element whose value
/// is shared), `distinct_values` the distinct texts among them and `stored_bytes` each distinct
/// text once plus 32 bytes for it (its allocation's two reference counts and its table slot, on
/// 64-bit targets): a reference itself costs nothing beyond the property slot a plain string
/// occupies too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct CompressionStats {
    /// Values stored compressed (`lz4`) or shared (`dedup`) at the rule's path (elements of the
    /// rule's label).
    pub compressed_values: u64,
    /// Their plain size in bytes.
    pub plain_bytes: u64,
    /// Their stored size in bytes: the compressed blocks without the dictionary (`lz4`), the
    /// distinct texts once with their overhead (`dedup`).
    pub stored_bytes: u64,
    /// The size of the rule's dictionary in bytes (0: none).
    pub dictionary_bytes: u64,
    /// The distinct values among `compressed_values` (`dedup`; 0 for `lz4`).
    #[cfg_attr(feature = "serde", serde(default))]
    pub distinct_values: u64,
}

// ---------------------------------------------------------------------------------------------
// TraversalGraph: write path, pass, load, invariant
// ---------------------------------------------------------------------------------------------

/// The labels of an edge as a list of zero or one.
fn edge_labels(label: Option<&str>) -> Vec<&str> {
    label.into_iter().collect()
}

impl TraversalGraph {
    /// Normalizes the properties of a vertex that is about to be stored.
    pub(crate) fn compression_prepare_vertex(
        &self,
        labels: &[CompactString],
        properties: &mut PropertyMap,
    ) {
        let labels: Vec<&str> = labels.iter().map(CompactString::as_str).collect();
        self.compression
            .normalize_element(ElementKind::Vertex, &labels, properties, false);
    }

    /// Normalizes the properties of an edge that is about to be stored.
    pub(crate) fn compression_prepare_edge(
        &self,
        label: Option<&str>,
        properties: &mut PropertyMap,
    ) {
        let labels: Vec<&str> = label.into_iter().collect();
        self.compression
            .normalize_element(ElementKind::Edge, &labels, properties, false);
    }

    /// Normalizes the stored top-level key `key` of a vertex in place (after a jpath write).
    pub(crate) fn compression_normalize_vertex_key(
        &mut self,
        node: crate::graph::VertexHandle,
        key: &str,
    ) {
        if self.compression.is_empty() {
            return;
        }
        let Some(vertex) = self
            .graph
            .node_weight_mut(node.index())
            .filter(|vertex| vertex.3 == node.generation())
        else {
            return;
        };
        let labels: Vec<&str> = self
            .labels
            .set(vertex.1)
            .iter()
            .map(CompactString::as_str)
            .collect();
        self.compression
            .normalize_key(ElementKind::Vertex, &labels, &mut vertex.2, key);
    }

    /// Normalizes the stored top-level key `key` of an edge in place.
    pub(crate) fn compression_normalize_edge_key(
        &mut self,
        edge: crate::graph::EdgeHandle,
        key: &str,
    ) {
        if self.compression.is_empty() {
            return;
        }
        let Some(edge) = self
            .graph
            .edge_weight_mut(edge.index())
            .filter(|weight| weight.3.own == edge.generation())
        else {
            return;
        };
        let labels = edge_labels(self.labels.name_str(edge.1));
        if let Some(properties) = edge.2.as_mut() {
            self.compression
                .normalize_key(ElementKind::Edge, &labels, properties, key);
        }
    }

    /// Normalizes every property of a vertex in place (after a label change or a restore).
    pub(crate) fn compression_normalize_vertex(&mut self, node: crate::graph::VertexHandle) {
        self.compression_normalize_vertex_forced(node, false);
    }

    fn compression_normalize_vertex_forced(
        &mut self,
        node: crate::graph::VertexHandle,
        force: bool,
    ) {
        let Some(vertex) = self
            .graph
            .node_weight_mut(node.index())
            .filter(|vertex| vertex.3 == node.generation())
        else {
            return;
        };
        let labels: Vec<&str> = self
            .labels
            .set(vertex.1)
            .iter()
            .map(CompactString::as_str)
            .collect();
        self.compression
            .normalize_element(ElementKind::Vertex, &labels, &mut vertex.2, force);
    }

    /// Normalizes every property of an edge in place.
    pub(crate) fn compression_normalize_edge(&mut self, edge: crate::graph::EdgeHandle) {
        self.compression_normalize_edge_forced(edge, false);
    }

    fn compression_normalize_edge_forced(&mut self, edge: crate::graph::EdgeHandle, force: bool) {
        let Some(weight) = self
            .graph
            .edge_weight_mut(edge.index())
            .filter(|weight| weight.3.own == edge.generation())
        else {
            return;
        };
        let labels = edge_labels(self.labels.name_str(weight.1));
        if let Some(properties) = weight.2.as_mut() {
            self.compression
                .normalize_element(ElementKind::Edge, &labels, properties, force);
        }
    }

    /// Rebuilds the rule state from the catalog (dictionaries of unchanged rules kept).
    fn compression_rebuild_rules(&mut self) {
        self.compression =
            CompressionRules::from_catalog(self.definitions.iter(), &self.compression);
    }

    /// The elements of `(element, label)` with their properties, in id order: the order the
    /// dictionary sample (and the estimate's sample, `compression_estimate.rs`) reads them in.
    pub(crate) fn compression_elements(
        &self,
        element: ElementKind,
        label: &str,
    ) -> Vec<(&str, &PropertyMap)> {
        let mut ids: Vec<(&str, &PropertyMap)> = Vec::new();
        match element {
            ElementKind::Vertex => {
                for node in self.index.get_nodes_by_label(label).into_iter().flatten() {
                    if let Some(vertex) = self.vertex_weight(node) {
                        ids.push((vertex.0.as_str(), &vertex.2));
                    }
                }
            }
            ElementKind::Edge => {
                for edge in self.index.get_edges_by_label(label).into_iter().flatten() {
                    if let Some(weight) = self.edge_weight(edge)
                        && let Some(properties) = weight.2.as_ref()
                    {
                        ids.push((weight.0.as_str(), properties));
                    }
                }
            }
        }
        ids.sort_unstable_by(|a, b| a.0.cmp(b.0));
        ids
    }

    /// Builds the dictionary of rule `name` from a sample of its current values: the values at
    /// its path of the elements of its label, in id order, at most 1 KiB of each, up to 64 KiB.
    fn compression_build_dictionary(&mut self, name: &str) {
        let Some(rule) = self.compression.find(name) else {
            return;
        };
        if !rule.use_dictionary {
            return;
        }
        let (element, label, path) = (rule.element, rule.label.clone(), rule.path.clone());
        let elements = self.compression_elements(element, &label);
        let dictionary = dictionary_from(&elements, &path);
        if let Some(rule) = self.compression.find_mut(name) {
            rule.dictionary = dictionary;
        }
    }

    /// Normalizes every element of `(element, label)` (`force`: re-encode with the current
    /// dictionaries).
    fn compression_normalize_label(&mut self, element: ElementKind, label: &str, force: bool) {
        match element {
            ElementKind::Vertex => {
                let nodes: Vec<_> = self
                    .index
                    .get_nodes_by_label(label)
                    .into_iter()
                    .flatten()
                    .collect();
                for node in nodes {
                    self.compression_normalize_vertex_forced(node, force);
                }
            }
            ElementKind::Edge => {
                let edges: Vec<_> = self
                    .index
                    .get_edges_by_label(label)
                    .into_iter()
                    .flatten()
                    .collect();
                for edge in edges {
                    self.compression_normalize_edge_forced(edge, force);
                }
            }
        }
    }

    /// The explicit pass after a catalog change of kind compression: `old` is what was stored
    /// before (`None`: nothing), the catalog already holds the new state. Rebuilds the rules,
    /// builds the dictionary of a new or changed rule and normalizes the affected labels.
    pub(crate) fn compression_after_change(&mut self, old: Option<&Definition>, name: &str) {
        let new_rule = self
            .definitions
            .get(DefinitionKind::COMPRESSION, name)
            .and_then(|definition| definition.as_compression().cloned())
            .filter(CompressionDefinition::is_served);
        let old_rule = old
            .and_then(|definition| definition.as_compression().cloned())
            .filter(CompressionDefinition::is_served);
        if new_rule.is_none() && old_rule.is_none() {
            return;
        }
        // A changed rule gets a new dictionary: drop the kept one first.
        if let Some(rule) = self.compression.find_mut(name) {
            rule.dictionary = None;
        }
        self.compression_rebuild_rules();
        if new_rule.is_some() {
            self.compression_build_dictionary(name);
        }
        let mut targets: Vec<(ElementKind, String)> = Vec::new();
        for rule in old_rule.iter().chain(new_rule.iter()) {
            let target = (rule.element(), rule.label().to_string());
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        for (element, label) in targets {
            self.compression_normalize_label(element, &label, true);
        }
    }

    /// The pass of a load (D11): rebuilds every rule from the catalog, builds every dictionary
    /// and normalizes every rule's label. Nothing happens without rules.
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn compression_load_pass(&mut self) {
        self.compression = CompressionRules::default();
        self.compression_rebuild_rules();
        if self.compression.is_empty() {
            return;
        }
        let names: Vec<CompactString> = self.compression.iter().map(|r| r.name.clone()).collect();
        for name in &names {
            self.compression_build_dictionary(name);
        }
        let mut targets: Vec<(ElementKind, CompactString)> = Vec::new();
        for rule in self.compression.iter() {
            let target = (rule.element, rule.label.clone());
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        for (element, label) in targets {
            self.compression_normalize_label(element, &label, true);
        }
    }

    /// Re-runs the pass of the compression rule `name`: rebuilds its dictionary from the
    /// current values and re-encodes its label. Changes no logical data, records nothing
    /// (D8). `Ok(false)`: no such rule.
    pub(crate) fn recompress_rule(&mut self, name: &str) -> bool {
        let Some(rule) = self.compression.find(name) else {
            return false;
        };
        let (element, label) = (rule.element, rule.label.clone());
        self.compression_build_dictionary(name);
        self.compression_normalize_label(element, &label, true);
        // A dedup table keeps only what the label's values reference.
        if let Some(table) = self.compression.find(name).and_then(|r| r.dedup.as_ref()) {
            table.lock().sweep();
        }
        true
    }

    /// How many entries the table of the `dedup` rule `name` holds, live or not yet swept
    /// (`None`: no such rule, or not a dedup rule). A test helper.
    #[doc(hidden)]
    pub fn dedup_table_len(&self, name: &str) -> Option<usize> {
        let table = self.compression.find(name)?.dedup.as_ref()?;
        Some(table.lock().len())
    }

    /// What the compression rule `name` holds right now (`None`: no such rule): the compressed
    /// values at its path among the elements of its label, their sizes, its dictionary.
    pub fn compression_stats(&self, name: &str) -> Option<CompressionStats> {
        let rule = self.compression.find(name)?;
        let mut stats = CompressionStats {
            dictionary_bytes: rule.dictionary.as_ref().map_or(0, |d| d.len() as u64),
            ..CompressionStats::default()
        };
        let mut seen: HashSet<*const u8> = HashSet::new();
        let mut count = |properties: &PropertyMap| match value_at(properties, &rule.path) {
            Some(ElementProperty::Compressed(text)) => {
                stats.compressed_values += 1;
                stats.plain_bytes += text.plain_len() as u64;
                stats.stored_bytes += text.stored_len() as u64;
            }
            Some(ElementProperty::Deduped(text)) => {
                stats.compressed_values += 1;
                stats.plain_bytes += text.len() as u64;
                if seen.insert(Arc::as_ptr(text.shared()).cast::<u8>()) {
                    stats.distinct_values += 1;
                    stats.stored_bytes += (text.len() + DEDUP_ENTRY_OVERHEAD) as u64;
                }
            }
            _ => {}
        };
        match rule.element {
            ElementKind::Vertex => {
                for node in self
                    .index
                    .get_nodes_by_label(&rule.label)
                    .into_iter()
                    .flatten()
                {
                    if let Some(vertex) = self.vertex_weight(node) {
                        count(&vertex.2);
                    }
                }
            }
            ElementKind::Edge => {
                for edge in self
                    .index
                    .get_edges_by_label(&rule.label)
                    .into_iter()
                    .flatten()
                {
                    if let Some(properties) = self.edge_weight(edge).and_then(|w| w.2.as_ref()) {
                        count(properties);
                    }
                }
            }
        }
        Some(stats)
    }

    /// Checks the compression invariant (D6) over the whole graph: a string at a rule path is
    /// compressed (`lz4`) or shared through the rule's table (`dedup`) iff it qualifies,
    /// nothing else is compressed or shared. `Err` names the first element and path that break
    /// it. A test helper; it decompresses and compresses as it goes.
    #[doc(hidden)]
    pub fn check_compression_invariant(&self) -> Result<(), String> {
        for (id, labels, properties, _) in self.graph.node_weights() {
            let labels: Vec<&str> = self
                .labels
                .set(*labels)
                .iter()
                .map(CompactString::as_str)
                .collect();
            self.check_element(ElementKind::Vertex, id, &labels, properties)?;
        }
        for (id, label, properties, _) in self.graph.edge_weights() {
            if let Some(properties) = properties {
                let labels = edge_labels(self.labels.name_str(*label));
                self.check_element(ElementKind::Edge, id, &labels, properties)?;
            }
        }
        // The rule state matches the catalog.
        let expected = CompressionRules::from_catalog(self.definitions.iter(), &self.compression);
        let names = |rules: &CompressionRules| {
            let mut names: Vec<String> = rules.iter().map(|r| r.name.to_string()).collect();
            names.sort();
            names
        };
        if names(&expected) != names(&self.compression) {
            return Err("the rule state differs from the catalog".to_string());
        }
        Ok(())
    }

    fn check_element(
        &self,
        element: ElementKind,
        id: &str,
        labels: &[&str],
        properties: &PropertyMap,
    ) -> Result<(), String> {
        for (key, value) in properties {
            let rules = self.compression.for_key(element, labels, key);
            check_value(value, &rules).map_err(|reason| {
                format!("{} '{id}', property '{key}': {reason}", element.as_str())
            })?;
        }
        Ok(())
    }
}

fn check_value(value: &ElementProperty, rules: &[(&[CompactString], &Rule)]) -> Result<(), String> {
    if let Some((_, rule)) = rules.iter().find(|(rest, _)| rest.is_empty()) {
        if rule.codec == Codec::Dedup {
            return check_dedup_value(value, rule);
        }
        return match value {
            ElementProperty::String(text) => match rule.encode(text) {
                Some(_) => Err(format!(
                    "a string of {} bytes that qualifies is stored plain",
                    text.len()
                )),
                None => Ok(()),
            },
            ElementProperty::Compressed(text) => {
                if text.plain_len() < rule.min_bytes {
                    Err("a value below min_bytes is compressed".to_string())
                } else if text.stored_len() * 100
                    > text.plain_len() * crate::value::compressed::MAX_RATIO_PERCENT
                {
                    Err("a compressed value exceeds 90 % of its plain size".to_string())
                } else {
                    Ok(())
                }
            }
            ElementProperty::Deduped(_) => Err("a shared value at an lz4 rule path".to_string()),
            other if other.holds_compressed() => {
                Err("a compressed value inside a non-string at a rule path".to_string())
            }
            _ => Ok(()),
        };
    }
    match value {
        ElementProperty::Compressed(_) => Err("a compressed value outside every rule".to_string()),
        ElementProperty::Deduped(_) => Err("a shared value outside every rule".to_string()),
        ElementProperty::Object(map) => {
            for (key, child) in map.iter() {
                let below: Vec<(&[CompactString], &Rule)> = rules
                    .iter()
                    .filter(|(rest, _)| rest.first().is_some_and(|first| first == key))
                    .map(|(rest, rule)| (&rest[1..], *rule))
                    .collect();
                check_value(child, &below).map_err(|reason| format!("{key}: {reason}"))?;
            }
            Ok(())
        }
        other if other.holds_compressed() => {
            Err("a compressed value outside every rule".to_string())
        }
        _ => Ok(()),
    }
}

/// [`check_value`] at the leaf of a `dedup` rule.
fn check_dedup_value(value: &ElementProperty, rule: &Rule) -> Result<(), String> {
    match value {
        ElementProperty::String(text) if rule.dedup_qualifies(text.len()) => Err(format!(
            "a string of {} bytes that qualifies for dedup is stored plain",
            text.len()
        )),
        ElementProperty::Deduped(shared) => {
            if !rule.dedup_qualifies(shared.len()) {
                Err("a value below min_bytes or inline is shared".to_string())
            } else if !rule
                .dedup
                .as_ref()
                .is_some_and(|table| table.lock().holds(shared.shared()))
            {
                Err("a shared value is not the copy in the rule's table".to_string())
            } else {
                Ok(())
            }
        }
        ElementProperty::Compressed(_) => {
            Err("an lz4-compressed value at a dedup rule path".to_string())
        }
        other if other.holds_compressed() => {
            Err("a compressed or shared value inside a non-string at a rule path".to_string())
        }
        _ => Ok(()),
    }
}

/// Checks a compression definition against the catalog it is about to join: no other rule of
/// the same `(element, label, path)` (D2).
pub(crate) fn check_conflict(
    graph: &TraversalGraph,
    definition: &Definition,
) -> Result<(), GraphError> {
    let Some(rule) = definition.as_compression() else {
        return Ok(());
    };
    for other in graph.definitions.iter() {
        if other.kind() != DefinitionKind::COMPRESSION || other.name() == definition.name() {
            continue;
        }
        // By name: a rule of a newer version (unknown element kind or path) conflicts with
        // a rule of the same text, so a newer version never finds two rules on one path.
        if let Some(existing) = other.as_compression()
            && existing.element_name() == rule.element_name()
            && existing.label() == rule.label()
            && existing.path_text() == rule.path_text()
        {
            return Err(DefinitionError::Conflict {
                kind: "compression rule".to_string(),
                name: definition.name().to_string(),
                existing: other.name().to_string(),
                reason: format!(
                    "the rule '{}' already covers {} '{}' at {}",
                    other.name(),
                    rule.element_name(),
                    rule.label(),
                    rule.path_text()
                ),
            }
            .into());
        }
    }
    Ok(())
}

impl Definition {
    /// A compression rule named `name` (kind [`DefinitionKind::COMPRESSION`], not critical:
    /// the data on disk is plain, so a reader that does not know the kind loses nothing but
    /// the compression).
    pub fn compression(name: impl Into<String>, rule: CompressionDefinition) -> Self {
        Definition::from_parts(
            DefinitionKind::COMPRESSION,
            crate::graph::definition::DefinitionFlags::NONE,
            name.into(),
            DefinitionBody::Compression(rule),
        )
    }

    /// The compression rule, when this is a decoded one.
    pub fn as_compression(&self) -> Option<&CompressionDefinition> {
        match self.body() {
            DefinitionBody::Compression(rule) => Some(rule),
            DefinitionBody::Query(_)
            | DefinitionBody::PropertyIndex(_)
            | DefinitionBody::Job(_)
            | DefinitionBody::Opaque(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphStorage;

    fn cv(seed: usize) -> String {
        let words = [
            "Senior engineer ",
            "led the platform team ",
            "built distributed systems ",
            "in Rust and Go ",
            "mentored juniors ",
        ];
        let mut out = format!("CV {seed}: ");
        for i in 0..40 {
            out.push_str(words[(seed + i) % words.len()]);
        }
        out
    }

    fn rule(path: &str) -> CompressionDefinition {
        CompressionDefinition::new(ElementKind::Vertex, "customer", path)
            .unwrap()
            .with_min_bytes(64)
    }

    #[test]
    fn paths_must_be_names() {
        assert!(CompressionDefinition::new(ElementKind::Vertex, "c", "a[0]").is_err());
        assert!(CompressionDefinition::new(ElementKind::Vertex, "c", "a[-1]").is_err());
        assert!(CompressionDefinition::new(ElementKind::Vertex, "c", "$").is_err());
        assert!(CompressionDefinition::new(ElementKind::Vertex, "c", "a.b").is_ok());
    }

    #[test]
    fn payload_round_trips() {
        let rule = rule("details.cv").with_dictionary(false);
        let back = CompressionDefinition::from_payload(&rule.to_payload()).unwrap();
        assert_eq!(back, rule);
    }

    #[test]
    fn define_write_and_drop_keep_the_invariant() {
        let mut graph = TraversalGraph::new();
        let mut props = PropertyMap::default();
        props.insert("bio", cv(1).into());
        let v = graph
            .insert_vertex(None, vec!["customer".into()], props)
            .unwrap();
        graph
            .set_definition(Definition::compression("bio", rule("bio")))
            .unwrap();
        graph.check_compression_invariant().unwrap();
        assert!(matches!(
            graph.vertex_properties(v).unwrap().unwrap().get("bio"),
            Some(ElementProperty::Compressed(_))
        ));
        assert_eq!(
            graph.vertex_properties(v).unwrap().unwrap().get("bio"),
            Some(&ElementProperty::from(cv(1)))
        );
        graph
            .set_vertex_property(v, "bio".into(), "short".into())
            .unwrap();
        graph.check_compression_invariant().unwrap();
        graph
            .set_vertex_property(v, "bio".into(), cv(2).into())
            .unwrap();
        graph.check_compression_invariant().unwrap();
        graph
            .remove_definition(DefinitionKind::COMPRESSION, "bio")
            .unwrap();
        graph.check_compression_invariant().unwrap();
        assert!(matches!(
            graph.vertex_properties(v).unwrap().unwrap().get("bio"),
            Some(ElementProperty::String(_))
        ));
    }
}
