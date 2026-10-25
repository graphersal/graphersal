//! Property indexes (`book/src/users_guide/property_indexes.md`; book page "Property Indexes"):
//! declared, ordered indexes `(element kind, label, key) -> value -> elements` that let a
//! `v()`/`e()` source with folded `has()` conditions read only the matching elements.
//!
//! An index is a catalog definition of kind [`DefinitionKind::PROPERTY_INDEX`]
//! ([`PropertyIndexDefinition`]), so it travels with the store like every definition (WAL
//! `SetDefinition`, snapshots, backups, forks, rollback). Its ENTRIES are never persisted: the
//! graph derives them from the data, like the label index and the statistics:
//!
//! - every mutation path of [`TraversalGraph`] that changes what an index holds (adding,
//!   dropping, relabelling an element, writing or removing a top-level property, a jpath write
//!   that changes its top-level key, and the undo of each of them) takes the element's entries
//!   out before the change and puts them back after it (`pindex_vertex` / `pindex_edge`); with
//!   no index declared that is one branch;
//! - a define, redefine, drop (and the undo of each) rebuilds that one index from the data;
//! - a load (`finish_load`) rebuilds every index after the elements and the catalog are in
//!   place, so a read-only open, a backup open and a recovery are consistent with the data.
//!
//! Keys are the LOGICAL scalar values (`null`, booleans, numbers, strings, uuids, dates and
//! times; a compressed string by its text). Arrays and objects are not indexed: a condition on one is never served
//! by an index. The index is ordered: an equality, `P.within` and the ranges `P.gt/gte/lt/lte/
//! inside/between/outside` read a slice of it. A lookup returns a SUPERSET of the matching
//! elements (numbers are ranged by their `float64` value, both `int64` and `float64` entries
//! included); the source re-checks every condition on each candidate, so a result never differs
//! from the scan's.
//!
//! Every index has a TYPE ([`PropertyIndexType`], payload key `type`): what kind of index it is
//! and which values it holds ([`PropertyIndexType::holds`], the one place of the matrix index
//! type x value type). This version knows `memory-ordered` (the default, the index described
//! above). A type written by a newer version is kept in the catalog and written back unchanged,
//! never damage, but never served: no entries are built, [`TraversalGraph`] does not offer it
//! in `property_indexes`, the optimizer never reads it, and listings say why
//! ([`PropertyIndexDefinition::unserved_reason`]). Creating an index is checked by
//! [`PropertyIndexDefinition::check_create`] (a known type, a key the schema does not declare
//! as values the type cannot hold); storing a definition (`set_definition`, replay,
//! `apply_change_set`, loads) never runs that check.
//!
//! An index may carry a TRANSFORM ([`PropertyIndexTransform`], payload key `transform`, absent
//! = none): its keys are then the transformed values. This version knows `to_lower` (the
//! lowered text of string values, exactly what the `to_lower()` step computes, through the one
//! shared function `step_to_lower::lower_text`; every other value is not in the index). It
//! serves `has(key, __.to_lower().is(P))` (case-insensitive search); the constants of `P` are
//! compared AS GIVEN with the lowered values. An unknown transform is handled like an unknown
//! type: kept byte for byte, never built or served, never created.

use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;

use compact_str::CompactString;

use crate::error::{DefinitionError, GraphError};
use crate::graph::definition::{Definition, DefinitionBody, DefinitionFlags, DefinitionKind};
use crate::graph::schema::{ElementKind, GraphSchema, JsonType, PropertySchema, ValueType};
use crate::graph::{EdgeHandle, EdgeIndex, NodeIndex, TraversalGraph, VertexHandle};
use crate::value::number::{N, canonicalize_f64};
use crate::value::temporal::Duration;
use crate::value::{ElementProperty, PropertyMap, Temporal};

/// The longest label of an index, in bytes.
pub const MAX_INDEX_LABEL_BYTES: usize = 1024;
/// The longest property key of an index, in bytes.
pub const MAX_INDEX_KEY_BYTES: usize = 1024;

// ---------------------------------------------------------------------------------------------
// The index type
// ---------------------------------------------------------------------------------------------

/// The type of a property index (payload key `type`, book page "Property Indexes", section
/// "Index types"): how the index keeps its entries, which values it holds
/// ([`Self::holds`]) and which conditions it serves.
///
/// This version knows one type, [`Self::MemoryOrdered`], the default when a definition names
/// none. A type name this version does not know (a definition written by a newer version) is
/// [`Self::Unknown`]: the definition is kept and written back unchanged, but the index is never
/// served, and creating one is refused ([`PropertyIndexDefinition::check_create`]).
///
/// ```
/// use graphersal::catalog::PropertyIndexType;
/// use graphersal::schema::ValueType;
///
/// let ordered = PropertyIndexType::from_name("memory-ordered");
/// assert_eq!(ordered, PropertyIndexType::MemoryOrdered);
/// assert_eq!(ordered.as_str(), "memory-ordered");
/// assert!(ordered.holds(&ValueType::Int64));
/// assert!(!ordered.holds(&ValueType::Object));
/// let future = PropertyIndexType::from_name("hash");
/// assert!(!future.is_known());
/// assert_eq!(future.as_str(), "hash");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PropertyIndexType {
    /// `"memory-ordered"`: an ordered index kept in memory, its entries derived from the data
    /// (never stored, rebuilt when a graph is loaded); it serves equality, `P.within` and the
    /// ranges. It holds the logical scalar values: `null`, `boolean`, `int64`, `float64`,
    /// `string` (a compressed one by its text), `uuid`, `date`, `localdatetime`, `datetime`,
    /// `time`, `duration`; never an array, an object or a vector.
    #[default]
    MemoryOrdered,
    /// A type name this version does not know, as read (a definition written by a newer
    /// version): kept, never served, never created.
    Unknown(String),
}

impl PropertyIndexType {
    /// The name of [`Self::MemoryOrdered`].
    pub const MEMORY_ORDERED: &'static str = "memory-ordered";

    /// The names of every type this version knows, in the order front ends offer them.
    pub const KNOWN_NAMES: [&'static str; 1] = [Self::MEMORY_ORDERED];

    /// The type of a name: a known one, else [`Self::Unknown`] with the name (never fails).
    pub fn from_name(name: &str) -> PropertyIndexType {
        match name {
            Self::MEMORY_ORDERED => PropertyIndexType::MemoryOrdered,
            other => PropertyIndexType::Unknown(other.to_string()),
        }
    }

    /// The type name as stored (`"memory-ordered"`, or the unknown name as read).
    pub fn as_str(&self) -> &str {
        match self {
            PropertyIndexType::MemoryOrdered => Self::MEMORY_ORDERED,
            PropertyIndexType::Unknown(name) => name,
        }
    }

    /// Whether this version knows the type (and so can build and serve the index).
    pub fn is_known(&self) -> bool {
        match self {
            PropertyIndexType::MemoryOrdered => true,
            PropertyIndexType::Unknown(_) => false,
        }
    }

    /// One sentence on what the type is, for front ends (a form's tooltip); `None` for an
    /// unknown type.
    pub fn description(&self) -> Option<&'static str> {
        match self {
            PropertyIndexType::MemoryOrdered => Some(
                "An ordered index kept in memory and rebuilt from the data when the graph is \
                 opened; it serves equality, P.within and ranges on scalar values.",
            ),
            PropertyIndexType::Unknown(_) => None,
        }
    }

    /// Whether an index of this type holds values of type `ty` (the matrix index type x value
    /// type of the book page "Property Indexes"): [`Self::MemoryOrdered`] holds `null`,
    /// `boolean`, `int64`, `float64`, `string`, `uuid` and the five temporal types, never an
    /// array, an object or a vector; a union is held when one of its members is. An unknown
    /// type holds nothing (it is not served).
    ///
    /// A value of a type the index does not hold is skipped (the element is simply not in the
    /// index), and a condition on such a value is answered by a scan.
    pub fn holds(&self, ty: &ValueType) -> bool {
        match self {
            PropertyIndexType::MemoryOrdered => match ty {
                ValueType::Null
                | ValueType::Boolean
                | ValueType::Int64
                | ValueType::Float64
                | ValueType::String
                | ValueType::Uuid
                | ValueType::Date
                | ValueType::LocalDateTime
                | ValueType::DateTime
                | ValueType::Time
                | ValueType::Duration
                | ValueType::OffsetTime => true,
                ValueType::Vector(_) | ValueType::Array(_) | ValueType::Object => false,
                ValueType::Union(members) => members.iter().any(|member| self.holds(member)),
            },
            PropertyIndexType::Unknown(_) => false,
        }
    }
}

impl std::fmt::Display for PropertyIndexType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------------------------
// The index transform
// ---------------------------------------------------------------------------------------------

/// The transform of a property index (payload key `transform`, absent = none; book page
/// "Property Indexes", section "Case-insensitive search"): the index keys are the transformed
/// values, and the index serves a condition that applies the same transform before its
/// predicate.
///
/// This version knows one transform, [`Self::ToLower`], named after the step `to_lower()`. A
/// transform name this version does not know (a definition written by a newer version) is
/// [`Self::Unknown`]: the definition is kept and written back unchanged, but the index is never
/// built or served, and creating one is refused ([`PropertyIndexDefinition::check_create`]).
///
/// ```
/// use graphersal::catalog::PropertyIndexTransform;
/// use graphersal::schema::ValueType;
///
/// let lower = PropertyIndexTransform::from_name("to_lower");
/// assert_eq!(lower, PropertyIndexTransform::ToLower);
/// assert_eq!(lower.as_str(), "to_lower");
/// assert!(lower.accepts(&ValueType::String));
/// assert!(!lower.accepts(&ValueType::Int64));
/// let future = PropertyIndexTransform::from_name("unaccent");
/// assert!(!future.is_known());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PropertyIndexTransform {
    /// `"to_lower"`: the key is the lowered text of a STRING value (a compressed one by its
    /// text), computed exactly as the step `to_lower()` computes it (Rust's `str::to_lowercase`:
    /// Unicode, no locale); a value of any other type is not in the index. Serves
    /// `has(key, __.to_lower().is(P))` with `P` an equality, `P.within` or a range.
    ToLower,
    /// A transform name this version does not know, as read: kept, never served, never created.
    Unknown(String),
}

impl PropertyIndexTransform {
    /// The name of [`Self::ToLower`].
    pub const TO_LOWER: &'static str = "to_lower";

    /// The names of every transform this version knows, in the order front ends offer them.
    pub const KNOWN_NAMES: [&'static str; 1] = [Self::TO_LOWER];

    /// The transform of a name: a known one, else [`Self::Unknown`] with the name (never fails).
    pub fn from_name(name: &str) -> PropertyIndexTransform {
        match name {
            Self::TO_LOWER => PropertyIndexTransform::ToLower,
            other => PropertyIndexTransform::Unknown(other.to_string()),
        }
    }

    /// The transform name as stored (`"to_lower"`, or the unknown name as read).
    pub fn as_str(&self) -> &str {
        match self {
            PropertyIndexTransform::ToLower => Self::TO_LOWER,
            PropertyIndexTransform::Unknown(name) => name,
        }
    }

    /// Whether this version knows the transform (and so can build and serve the index).
    pub fn is_known(&self) -> bool {
        match self {
            PropertyIndexTransform::ToLower => true,
            PropertyIndexTransform::Unknown(_) => false,
        }
    }

    /// One sentence on what the transform does, for front ends; `None` for an unknown one.
    pub fn description(&self) -> Option<&'static str> {
        match self {
            PropertyIndexTransform::ToLower => Some(
                "Indexes the lowered text of string values (as to_lower() computes it) for \
                 case-insensitive search: query it with has(key, __.to_lower().is(..)).",
            ),
            PropertyIndexTransform::Unknown(_) => None,
        }
    }

    /// Whether the transform takes values of type `ty` (a union when one of its members is):
    /// [`Self::ToLower`] takes strings only; an unknown transform takes nothing.
    pub fn accepts(&self, ty: &ValueType) -> bool {
        match ty {
            ValueType::Union(members) => members.iter().any(|member| self.accepts(member)),
            _ => match self {
                PropertyIndexTransform::ToLower => *ty == ValueType::String,
                PropertyIndexTransform::Unknown(_) => false,
            },
        }
    }

    /// The transformed value of a stored value, `None` when the transform does not take it
    /// (the element is then not in the index).
    pub(crate) fn apply(&self, value: &ElementProperty) -> Option<ElementProperty> {
        match self {
            PropertyIndexTransform::ToLower => value.text().map(|text| {
                ElementProperty::String(
                    crate::traversal::step::map_steps::step_to_lower::lower_text(&text).into(),
                )
            }),
            PropertyIndexTransform::Unknown(_) => None,
        }
    }
}

impl std::fmt::Display for PropertyIndexTransform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The index key of a stored value under an optional transform; `None` when the index does not
/// hold the value.
fn transformed_key(
    transform: Option<&PropertyIndexTransform>,
    value: &ElementProperty,
) -> Option<IndexKey> {
    match transform {
        None => IndexKey::of(value),
        Some(transform) => IndexKey::of(&transform.apply(value)?),
    }
}

/// The value types a schema node declares (through `anyOf`), `None` when it declares no type
/// restriction (no `type` and no `anyOf`, or an `anyOf` member without one).
fn declared_value_types(node: &PropertySchema) -> Option<Vec<ValueType>> {
    if let Some(types) = &node.types {
        return Some(
            types
                .iter()
                .map(|ty| match ty {
                    JsonType::Null => ValueType::Null,
                    JsonType::Boolean => ValueType::Boolean,
                    JsonType::Integer => ValueType::Int64,
                    JsonType::Number => ValueType::Float64,
                    JsonType::String => match node.format {
                        None => ValueType::String,
                        Some(format) => match format.temporal_kind() {
                            Some(kind) => ValueType::of_temporal(kind),
                            None => ValueType::Uuid,
                        },
                    },
                    JsonType::Array => match &node.vector {
                        Some(vector) => ValueType::Vector(vector.element),
                        None => ValueType::Array(Box::new(ValueType::Union(Vec::new()))),
                    },
                    JsonType::Object => ValueType::Object,
                })
                .collect(),
        );
    }
    let members = node.any_of.as_ref()?;
    let mut types = Vec::new();
    for member in members {
        types.extend(declared_value_types(member)?);
    }
    Some(types)
}

// ---------------------------------------------------------------------------------------------
// The definition
// ---------------------------------------------------------------------------------------------

/// A property index (catalog kind [`DefinitionKind::PROPERTY_INDEX`]): the `element`s labelled
/// `label`, ordered by the value of their top-level property `key`, in an index of type
/// [`index_type`](Self::index_type) (default [`PropertyIndexType::MemoryOrdered`]).
///
/// ```
/// use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexType};
/// use graphersal::storage::ElementKind;
///
/// let index = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "title");
/// assert_eq!(index.label(), "person");
/// assert_eq!(index.key(), "title");
/// assert_eq!(index.index_type(), &PropertyIndexType::MemoryOrdered);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyIndexDefinition {
    element: ElementKind,
    label: String,
    key: String,
    index_type: PropertyIndexType,
    transform: Option<PropertyIndexTransform>,
    extra: PropertyMap,
    /// The payload map exactly as read, kept for an UNKNOWN type or transform only: written back unchanged
    /// (the key order and every key of a newer version), so the definition survives byte for
    /// byte.
    original: Option<PropertyMap>,
    /// An element kind name this version does not know, as read (the index is kept, never
    /// built or served; `element` is a placeholder).
    unknown_element: Option<String>,
}

impl PropertyIndexDefinition {
    /// An index of the `element`s labelled `label` on their top-level property `key` (a plain
    /// name, never a jpath). Checked by [`Definition::validate`].
    pub fn new(element: ElementKind, label: impl Into<String>, key: impl Into<String>) -> Self {
        PropertyIndexDefinition {
            element,
            label: label.into(),
            key: key.into(),
            index_type: PropertyIndexType::MemoryOrdered,
            transform: None,
            extra: PropertyMap::default(),
            original: None,
            unknown_element: None,
        }
    }

    /// The same index with the type `index_type`. Only a known type can be created
    /// ([`Self::check_create`]).
    ///
    /// ```
    /// use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexType};
    /// use graphersal::storage::ElementKind;
    ///
    /// let index = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age")
    ///     .with_type(PropertyIndexType::MemoryOrdered);
    /// assert_eq!(index.index_type().as_str(), "memory-ordered");
    /// ```
    pub fn with_type(mut self, index_type: PropertyIndexType) -> Self {
        // An index of an unknown element kind is kept as read.
        if self.unknown_element.is_none() {
            self.index_type = index_type;
            self.original = None;
        }
        self
    }

    /// The type of the index.
    pub fn index_type(&self) -> &PropertyIndexType {
        &self.index_type
    }

    /// The same index with the transform `transform` (`None`: no transform, the default). Only
    /// a known transform can be created ([`Self::check_create`]).
    ///
    /// ```
    /// use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexTransform};
    /// use graphersal::storage::ElementKind;
    ///
    /// let index = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name")
    ///     .with_transform(Some(PropertyIndexTransform::ToLower));
    /// assert_eq!(index.transform(), Some(&PropertyIndexTransform::ToLower));
    /// assert_eq!(index.default_name(), "person_name_to_lower");
    /// ```
    pub fn with_transform(mut self, transform: Option<PropertyIndexTransform>) -> Self {
        // An index of an unknown element kind is kept as read.
        if self.unknown_element.is_none() {
            self.transform = transform;
            self.original = None;
        }
        self
    }

    /// The transform of the index, `None` when it indexes the values themselves.
    pub fn transform(&self) -> Option<&PropertyIndexTransform> {
        self.transform.as_ref()
    }

    /// Whether the index holds values of type `ty`: its type holds them
    /// ([`PropertyIndexType::holds`]) and its transform, if any, takes them
    /// ([`PropertyIndexTransform::accepts`]: `to_lower` takes strings only); a union when one of
    /// its members is held.
    ///
    /// ```
    /// use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexTransform};
    /// use graphersal::schema::ValueType;
    /// use graphersal::storage::ElementKind;
    ///
    /// let plain = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "name");
    /// let lower = plain.clone().with_transform(Some(PropertyIndexTransform::ToLower));
    /// assert!(plain.holds(&ValueType::Int64));
    /// assert!(!lower.holds(&ValueType::Int64));
    /// assert!(lower.holds(&ValueType::Union(vec![ValueType::Int64, ValueType::String])));
    /// ```
    pub fn holds(&self, ty: &ValueType) -> bool {
        match ty {
            ValueType::Union(members) => members.iter().any(|member| self.holds(member)),
            _ => {
                self.index_type.holds(ty)
                    && self
                        .transform
                        .as_ref()
                        .is_none_or(|transform| transform.accepts(ty))
            }
        }
    }

    /// Why a storage that serves property indexes does not serve this one, `None` when it can:
    /// an index of a type this version does not know (`"index type 'x' is not known to this
    /// version ..."`). Listings show it next to `served: false`.
    pub fn unserved_reason(&self) -> Option<String> {
        if let Some(element) = &self.unknown_element {
            return Some(format!(
                "element kind '{element}' is not known to this version (a newer Graphersal \
                 wrote it); the definition is kept, queries scan"
            ));
        }
        if let PropertyIndexType::Unknown(name) = &self.index_type {
            return Some(format!(
                "index type '{name}' is not known to this version (a newer Graphersal wrote \
                 it); the definition is kept, queries scan"
            ));
        }
        match &self.transform {
            Some(PropertyIndexTransform::Unknown(name)) => Some(format!(
                "index transform '{name}' is not known to this version (a newer Graphersal \
                 wrote it); the definition is kept, queries scan"
            )),
            Some(PropertyIndexTransform::ToLower) | None => None,
        }
    }

    /// Whether this version builds and serves the index (its type and transform are known).
    pub(crate) fn is_served_type(&self) -> bool {
        self.unknown_element.is_none()
            && self.index_type.is_known()
            && self
                .transform
                .as_ref()
                .is_none_or(PropertyIndexTransform::is_known)
    }

    /// Checks that the index can be CREATED (named `name`, for the error) in a graph whose
    /// stored schema is `schema`: run by every front end that creates an index
    /// ([`GraphTraversalSource::create_index`](crate::GraphTraversalSource::create_index), the
    /// DSL's `g.create_index`, the session), never by `set_definition`, replay,
    /// `apply_change_set` or a load, which store what they carry.
    ///
    /// - The type must be known: an unknown one is [`DefinitionError::InvalidField`] with the
    ///   field `type`; so must the transform, if any (field `transform`).
    /// - When `schema` declares the key at the top level of the index's label (of its element
    ///   kind) and none of the declared types except `null` can be held by the index
    ///   ([`Self::holds`]: for the type a vector, an array, an object, or a union of only such;
    ///   with `to_lower` every type but `string`), it is
    ///   [`DefinitionError::IndexTypeMismatch`]. A declaration with at least one held
    ///   type, no declaration and no schema are fine (values the index cannot hold are
    ///   skipped). The schema's mode does not matter. A later schema change never drops an
    ///   index.
    ///
    /// ```
    /// use graphersal::catalog::PropertyIndexDefinition;
    /// use graphersal::schema::{GraphSchema, PropertySchema};
    /// use graphersal::storage::ElementKind;
    ///
    /// let schema = GraphSchema::new().with_vertex(
    ///     "person",
    ///     PropertySchema::object()
    ///         .with_property("tags", PropertySchema::array(PropertySchema::string())),
    /// );
    /// let tags = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "tags");
    /// assert!(tags.check_create("person_tags", Some(&schema)).is_err());
    /// let age = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age");
    /// assert!(age.check_create("person_age", Some(&schema)).is_ok());
    /// assert!(tags.check_create("person_tags", None).is_ok());
    /// ```
    pub fn check_create(
        &self,
        name: &str,
        schema: Option<&GraphSchema>,
    ) -> Result<(), DefinitionError> {
        if let Some(element) = &self.unknown_element {
            return Err(DefinitionError::InvalidField {
                name: name.to_string(),
                field: "element".to_string(),
                reason: format!(
                    "'{element}' is not an element kind this version knows (known: vertex, edge)"
                ),
            });
        }
        if let PropertyIndexType::Unknown(type_name) = &self.index_type {
            return Err(DefinitionError::InvalidField {
                name: name.to_string(),
                field: "type".to_string(),
                reason: format!(
                    "'{type_name}' is not an index type this version knows (known: {})",
                    PropertyIndexType::KNOWN_NAMES.join(", ")
                ),
            });
        }
        if let Some(PropertyIndexTransform::Unknown(transform)) = &self.transform {
            return Err(DefinitionError::InvalidField {
                name: name.to_string(),
                field: "transform".to_string(),
                reason: format!(
                    "'{transform}' is not an index transform this version knows (known: {})",
                    PropertyIndexTransform::KNOWN_NAMES.join(", ")
                ),
            });
        }
        let Some(schema) = schema else {
            return Ok(());
        };
        let root = match self.element {
            ElementKind::Vertex => schema.vertices.get(&self.label).map(|v| &v.schema),
            ElementKind::Edge => schema.edges.get(&self.label).map(|e| &e.schema),
        };
        let Some(node) = root.and_then(|root| root.properties.get(&self.key)) else {
            return Ok(());
        };
        let Some(types) = declared_value_types(node) else {
            return Ok(());
        };
        let mut non_null = types.iter().filter(|ty| **ty != ValueType::Null).peekable();
        if non_null.peek().is_none() || non_null.any(|ty| self.holds(ty)) {
            return Ok(());
        }
        Err(DefinitionError::IndexTypeMismatch {
            element: self.element,
            label: self.label.clone(),
            key: self.key.clone(),
            declared: node.non_null_type_name(),
            index_type: self.index_type.as_str().into(),
            transform: self.transform.as_ref().map(|t| t.as_str().into()),
        })
    }

    /// The element kind the index covers (`Vertex` for an index whose element kind this
    /// version does not know: see [`Self::element_name`]).
    pub fn element(&self) -> ElementKind {
        self.element
    }

    /// The element kind's name (`"vertex"`, `"edge"`), or the unknown name as read (an index
    /// of a newer version, kept and not served: [`Self::unserved_reason`]).
    pub fn element_name(&self) -> &str {
        match &self.unknown_element {
            Some(name) => name,
            None => self.element.as_str(),
        }
    }

    /// The label the index covers (it need not exist yet).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The top-level property key the index orders by.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The name an index gets when none is given (the DSL's `g.create_index` without `name`,
    /// the session, MCP, the playground's form): `<label>_<key>` (`<label>_<key>_edge` for an
    /// edge index), then `_<transform>` for an index with a transform (`person_name_to_lower`,
    /// `knows_note_edge_to_lower`), every character that is not an ASCII letter, digit or `_`
    /// replaced by `_`, prefixed with `index_` when it would not start with a letter.
    ///
    /// ```
    /// use graphersal::catalog::PropertyIndexDefinition;
    /// use graphersal::storage::ElementKind;
    ///
    /// let index = PropertyIndexDefinition::new(ElementKind::Edge, "knows", "weight");
    /// assert_eq!(index.default_name(), "knows_weight_edge");
    /// let index = PropertyIndexDefinition::new(ElementKind::Vertex, "2nd-tier", "e-mail");
    /// assert_eq!(index.default_name(), "index_2nd_tier_e_mail");
    /// ```
    pub fn default_name(&self) -> String {
        let mut name: String = format!("{}_{}", self.label, self.key)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        if self.element == ElementKind::Edge {
            name.push_str("_edge");
        }
        if let Some(transform) = &self.transform {
            name.push('_');
            name.extend(
                transform
                    .as_str()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }),
            );
        }
        if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
            name.insert_str(0, "index_");
        }
        name
    }

    /// Payload keys this version does not know, kept and written back.
    pub fn extra(&self) -> &PropertyMap {
        &self.extra
    }

    /// The payload property map (format spec 6.3), in this key order: `element` (`"vertex"` or
    /// `"edge"`), `label`, `key`, `type` (always written), `transform` (only for an index with
    /// a transform), then the unknown keys. A definition of an unknown type or transform read
    /// from a payload gives that payload back unchanged.
    pub fn to_payload(&self) -> PropertyMap {
        if let Some(original) = &self.original {
            return original.clone();
        }
        let mut map = PropertyMap::default();
        map.insert("element", self.element.as_str().into());
        map.insert("label", self.label.as_str().into());
        map.insert("key", self.key.as_str().into());
        map.insert("type", self.index_type.as_str().into());
        if let Some(transform) = &self.transform {
            map.insert("transform", transform.as_str().into());
        }
        for (key, value) in &self.extra {
            map.insert_if_absent(key, || value.clone());
        }
        map
    }

    /// Reads a payload map (the inverse of [`Self::to_payload`]). A missing `element` is
    /// `"vertex"`, a missing `type` is `"memory-ordered"`, a missing `transform` is none; a
    /// known key of the wrong type is an error; unknown keys are kept. A type or transform this
    /// version does not know is [`PropertyIndexType::Unknown`] /
    /// [`PropertyIndexTransform::Unknown`] (never an error), and the whole map is kept to be
    /// written back unchanged. Not validated.
    pub fn from_payload(payload: &PropertyMap) -> Result<PropertyIndexDefinition, DefinitionError> {
        Self::read_fields(payload, &[])
    }

    pub(crate) fn read_fields(
        map: &PropertyMap,
        skip: &[&str],
    ) -> Result<PropertyIndexDefinition, DefinitionError> {
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
        let mut unknown_element = None;
        let element = match text("element")?.as_deref() {
            None | Some("vertex") => ElementKind::Vertex,
            Some("edge") => ElementKind::Edge,
            Some(other) => {
                unknown_element = Some(other.to_string());
                ElementKind::Vertex
            }
        };
        let label = text("label")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'label' is missing".to_string(),
        })?;
        let key = text("key")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'key' is missing".to_string(),
        })?;
        let index_type = match text("type")? {
            None => PropertyIndexType::MemoryOrdered,
            Some(name) => PropertyIndexType::from_name(&name),
        };
        let transform = text("transform")?.map(|name| PropertyIndexTransform::from_name(&name));
        let mut index = PropertyIndexDefinition::new(element, label, key)
            .with_type(index_type)
            .with_transform(transform);
        index.unknown_element = unknown_element;
        const KNOWN: [&str; 5] = ["element", "label", "key", "type", "transform"];
        for (key, value) in map {
            if !KNOWN.contains(&key.as_str()) && !skip.contains(&key.as_str()) {
                index.extra.insert(key.clone(), value.to_logical());
            }
        }
        if !index.is_served_type() {
            let mut original = PropertyMap::default();
            for (key, value) in map {
                if !skip.contains(&key.as_str()) {
                    original.insert(key.clone(), value.to_logical());
                }
            }
            index.original = Some(original);
        }
        Ok(index)
    }

    /// Writes the exchange-form fields (the payload keys) into `map`.
    pub(crate) fn write_fields(&self, map: &mut PropertyMap) {
        for (key, value) in self.to_payload() {
            map.insert(key, value);
        }
    }

    /// The rules of [`Definition::validate`] for an index named `name`.
    pub(crate) fn validate(&self, name: &str) -> Result<(), DefinitionError> {
        let field = |field: &str, reason: String| DefinitionError::InvalidField {
            name: name.to_string(),
            field: field.to_string(),
            reason,
        };
        if self.label.is_empty() {
            return Err(field("label", "it is empty".to_string()));
        }
        if self.label.len() > MAX_INDEX_LABEL_BYTES {
            return Err(field(
                "label",
                format!("it is longer than {MAX_INDEX_LABEL_BYTES} bytes"),
            ));
        }
        if self.label.contains("::") {
            return Err(field(
                "label",
                "a label cannot contain '::' (the multi-label separator)".to_string(),
            ));
        }
        if self.key.is_empty() {
            return Err(field(
                "key",
                "it is empty; name the top-level property, for example \"title\"".to_string(),
            ));
        }
        if self.key.len() > MAX_INDEX_KEY_BYTES {
            return Err(field(
                "key",
                format!("it is longer than {MAX_INDEX_KEY_BYTES} bytes"),
            ));
        }
        if let Some(key) = self
            .extra
            .keys()
            .find(|key| crate::graph::definition::RESERVED_PAYLOAD_KEYS.contains(&key.as_str()))
        {
            return Err(field(
                "payload",
                format!("the payload key '{key}' is reserved"),
            ));
        }
        Ok(())
    }
}

impl Definition {
    /// A property index named `name` (kind [`DefinitionKind::PROPERTY_INDEX`], not critical:
    /// the index holds no data of its own, a reader that does not know the kind loses nothing
    /// but the speed-up).
    pub fn property_index(name: impl Into<String>, index: PropertyIndexDefinition) -> Self {
        Definition::from_parts(
            DefinitionKind::PROPERTY_INDEX,
            DefinitionFlags::NONE,
            name.into(),
            DefinitionBody::PropertyIndex(index),
        )
    }

    /// The property index, when this is a decoded one.
    pub fn as_property_index(&self) -> Option<&PropertyIndexDefinition> {
        match self.body() {
            DefinitionBody::PropertyIndex(index) => Some(index),
            DefinitionBody::Query(_)
            | DefinitionBody::Compression(_)
            | DefinitionBody::Job(_)
            | DefinitionBody::Opaque(_) => None,
        }
    }
}

/// Checks a property index definition against the catalog it is about to join: no other index
/// of the same `(element, label, key, type, transform)` (indexes of different types or
/// transforms on one key, such as a plain one and a `to_lower` one, may coexist).
pub(crate) fn check_conflict(
    graph: &TraversalGraph,
    definition: &Definition,
) -> Result<(), GraphError> {
    let Some(index) = definition.as_property_index() else {
        return Ok(());
    };
    for other in graph.definitions.iter() {
        if other.kind() != DefinitionKind::PROPERTY_INDEX || other.name() == definition.name() {
            continue;
        }
        if let Some(existing) = other.as_property_index()
            && existing.element_name() == index.element_name()
            && existing.label() == index.label()
            && existing.key() == index.key()
            && existing.index_type() == index.index_type()
            && existing.transform() == index.transform()
        {
            return Err(DefinitionError::Conflict {
                kind: "property index".to_string(),
                name: definition.name().to_string(),
                existing: other.name().to_string(),
                reason: format!(
                    "the index '{}' already covers {} '{}' on '{}' (type '{}'{})",
                    other.name(),
                    index.element_name(),
                    index.label(),
                    index.key(),
                    index.index_type(),
                    match index.transform() {
                        Some(transform) => format!(", transform '{transform}'"),
                        None => String::new(),
                    }
                ),
            }
            .into());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Public read types
// ---------------------------------------------------------------------------------------------

/// What one property index holds right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PropertyIndexStats {
    /// Indexed elements (one entry per element with a scalar value at the key).
    pub entries: u64,
    /// Distinct indexed values.
    pub distinct_values: u64,
    /// An estimate of the index's heap footprint, in bytes.
    pub bytes: u64,
    /// Elements of the index's label whose value at the key is not `null` and not held by the
    /// index (an array, an object or a vector; with `to_lower` every non-string value). The
    /// optimizer serves a transformed index only while this is `0`: the transform's step fails
    /// on such a value (`to_lower()` on a number), and a scan must report that error, so a
    /// storage serving an index with a transform must count them.
    #[cfg_attr(feature = "serde", serde(default))]
    pub skipped: u64,
}

impl PropertyIndexStats {
    /// Stats with these figures (for a storage outside this crate), `skipped` 0 (see
    /// [`Self::with_skipped`]).
    pub fn new(entries: u64, distinct_values: u64, bytes: u64) -> Self {
        PropertyIndexStats {
            entries,
            distinct_values,
            bytes,
            skipped: 0,
        }
    }

    /// The same stats with `skipped` elements whose value the index does not hold.
    pub fn with_skipped(mut self, skipped: u64) -> Self {
        self.skipped = skipped;
        self
    }
}

/// A property index a storage serves ([`GraphStorage::property_indexes`]): its name, what it
/// covers and its current stats.
///
/// [`GraphStorage::property_indexes`]: crate::graph::GraphStorage::property_indexes
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PropertyIndexInfo {
    /// The name of the index definition.
    pub name: String,
    /// The element kind it covers.
    pub element: ElementKind,
    /// The label it covers.
    pub label: String,
    /// The top-level property key it orders by.
    pub key: String,
    /// The type of the index (a storage offers only indexes of types it serves).
    pub index_type: PropertyIndexType,
    /// The transform of the index (`None`: the values themselves).
    pub transform: Option<PropertyIndexTransform>,
    /// What it holds right now.
    pub stats: PropertyIndexStats,
}

impl PropertyIndexInfo {
    /// An entry with these parts (for a storage outside this crate), of type
    /// [`PropertyIndexType::MemoryOrdered`] (see [`Self::with_type`]).
    pub fn new(
        name: impl Into<String>,
        element: ElementKind,
        label: impl Into<String>,
        key: impl Into<String>,
        stats: PropertyIndexStats,
    ) -> Self {
        PropertyIndexInfo {
            name: name.into(),
            element,
            label: label.into(),
            key: key.into(),
            index_type: PropertyIndexType::MemoryOrdered,
            transform: None,
            stats,
        }
    }

    /// The same entry with the type `index_type`.
    pub fn with_type(mut self, index_type: PropertyIndexType) -> Self {
        self.index_type = index_type;
        self
    }

    /// The same entry with the transform `transform`.
    pub fn with_transform(mut self, transform: Option<PropertyIndexTransform>) -> Self {
        self.transform = transform;
        self
    }
}

/// The slice of a property index a lookup reads
/// ([`GraphStorage::vertices_by_property_index`]): the values between `lower` and `upper`.
///
/// The bounds are scalar values (`null`, boolean, number, string, uuid, or one temporal type:
/// `date`, `localdatetime`, `datetime`, `time`, `duration`) of ONE kind; values of other kinds
/// are never in the slice. Numbers compare by value across `int64` and `float64`, strings
/// bytewise, booleans `false < true`, uuids by their 128-bit word, a `datetime` by its instant
/// (the offset is not compared), a `duration` by its length then its fields, as the predicates
/// do.
/// A storage may return MORE elements than the slice holds (the caller re-checks every
/// condition on each candidate), never fewer. An equality ([`Self::equal`]) is the slice
/// `[v, v]`.
///
/// [`GraphStorage::vertices_by_property_index`]: crate::graph::GraphStorage::vertices_by_property_index
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PropertyIndexRange {
    /// The lower bound.
    pub lower: Bound<ElementProperty>,
    /// The upper bound.
    pub upper: Bound<ElementProperty>,
}

impl PropertyIndexRange {
    /// The slice between `lower` and `upper`.
    pub fn new(lower: Bound<ElementProperty>, upper: Bound<ElementProperty>) -> Self {
        PropertyIndexRange { lower, upper }
    }

    /// The values equal to `value`.
    pub fn equal(value: ElementProperty) -> Self {
        PropertyIndexRange {
            lower: Bound::Included(value.clone()),
            upper: Bound::Included(value),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The ordered key
// ---------------------------------------------------------------------------------------------

/// The ordered key of an index entry: a logical scalar. Kinds are ordered `null < boolean <
/// number < string < uuid < date < localdatetime < datetime < time < duration`; within a kind
/// the order of the predicates (numbers by their canonical `float64` value first, then `int64`
/// before `float64`, then the exact `int64`; temporal values by `Temporal`'s own order).
///
/// Equal keys are exactly the values `has()` finds equal: each temporal type is its own kind
/// (a `date` never equals a `datetime` or a number), a `datetime` key is its INSTANT
/// (`12:00Z` and `14:00+02:00` are one key), and a `duration` key is the whole [`Duration`],
/// whose equality is field by field (`P1M` is not `P30D`) and whose order is its length (a month
/// as 30 days) with ties broken by the fields: an order consistent with that equality, so
/// equality, `P.within` and ranges on durations are all exact slices.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum IndexKey {
    Null,
    Bool(bool),
    /// `(canonical float64 value, 0 = int64 / 1 = float64, the int64 or 0)`.
    Number(crate::value::number::OrderedF64, u8, i64),
    Str(CompactString),
    /// The uuid's 128-bit word as `(high, low)` (keeps the key 8-aligned).
    Uuid(u64, u64),
    /// Days since 1970-01-01.
    Date(i32),
    /// Wall-clock nanoseconds since 1970-01-01T00:00:00 as `(whole microseconds rounded down,
    /// the nanoseconds above them 0..=999)`: ordered like the nanoseconds, the key stays
    /// 8-aligned.
    LocalDateTime(i64, u16),
    /// The instant as `(microseconds since 1970-01-01T00:00:00Z rounded down, the nanoseconds
    /// above them)` (the offset is not part of the key).
    DateTime(i64, u16),
    /// Nanoseconds after midnight.
    Time(i64),
    /// The duration itself (its `Ord` is the length, then the fields).
    Duration(Duration),
    /// The UTC time of day in nanoseconds (`OffsetTime::utc_nanos`; the offset is not part of
    /// the key).
    OffsetTime(i64),
}

/// The kinds of [`IndexKey`], in key order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    Null,
    Bool,
    Number,
    Str,
    Uuid,
    Date,
    LocalDateTime,
    DateTime,
    Time,
    Duration,
    OffsetTime,
}

impl IndexKey {
    /// The key of a stored (or queried) value; `None` for a value that is not indexed (arrays,
    /// objects, elements, maps).
    fn of(value: &ElementProperty) -> Option<IndexKey> {
        match value {
            ElementProperty::Null => Some(IndexKey::Null),
            ElementProperty::Boolean(flag) => Some(IndexKey::Bool(*flag)),
            ElementProperty::Number(number) => Some(match &number.n {
                N::NegInt(int) => IndexKey::Number(canonicalize_f64(*int as f64), 0, *int),
                N::Float(float) => IndexKey::Number(canonicalize_f64(float.0), 1, 0),
            }),
            ElementProperty::String(text) => Some(IndexKey::Str(text.clone())),
            ElementProperty::Compressed(text) => Some(IndexKey::Str(text.text())),
            ElementProperty::Deduped(text) => Some(IndexKey::Str(text.as_str().into())),
            ElementProperty::Uuid(bits) => Some(IndexKey::Uuid((*bits >> 64) as u64, *bits as u64)),
            ElementProperty::Temporal(value) => Some(match *value {
                Temporal::Date(date) => IndexKey::Date(date.days_since_epoch()),
                Temporal::LocalDateTime(local) => {
                    IndexKey::LocalDateTime(local.micros(), sub_micros(local.nanos()))
                }
                Temporal::DateTime(instant) => {
                    IndexKey::DateTime(instant.epoch_micros(), sub_micros(instant.epoch_nanos()))
                }
                Temporal::Time(time) => IndexKey::Time(time.nanos_of_day()),
                Temporal::Duration(duration) => IndexKey::Duration(duration),
                Temporal::OffsetTime(time) => IndexKey::OffsetTime(time.utc_nanos()),
            }),
            // A vector is not an index key (book `users_guide/vectors.md`): `has(key, vector)` finds it
            // by a scan, with value equality.
            ElementProperty::Vector(_)
            | ElementProperty::Object(_)
            | ElementProperty::Array(_)
            | ElementProperty::Vertex(_)
            | ElementProperty::Edge(_)
            | ElementProperty::Map(_) => None,
        }
    }

    fn kind(&self) -> KeyKind {
        match self {
            IndexKey::Null => KeyKind::Null,
            IndexKey::Bool(_) => KeyKind::Bool,
            IndexKey::Number(..) => KeyKind::Number,
            IndexKey::Str(_) => KeyKind::Str,
            IndexKey::Uuid(..) => KeyKind::Uuid,
            IndexKey::Date(_) => KeyKind::Date,
            IndexKey::LocalDateTime(..) => KeyKind::LocalDateTime,
            IndexKey::DateTime(..) => KeyKind::DateTime,
            IndexKey::Time(_) => KeyKind::Time,
            IndexKey::Duration(_) => KeyKind::Duration,
            IndexKey::OffsetTime(_) => KeyKind::OffsetTime,
        }
    }

    /// The heap bytes the key owns.
    fn heap_bytes(&self) -> u64 {
        match self {
            IndexKey::Str(text) if text.is_heap_allocated() => text.capacity() as u64,
            _ => 0,
        }
    }
}

/// Whether a value can be an index key (a scalar: `null`, boolean, number, string, uuid, a date
/// or time; exactly the values whose type [`PropertyIndexType::MemoryOrdered`]
/// [holds](PropertyIndexType::holds), checked by a unit test); a condition on any other value is
/// never served by an index.
pub(crate) fn is_indexable(value: &ElementProperty) -> bool {
    IndexKey::of(value).is_some()
}

/// The first entry of a key kind (inclusive).
fn kind_start(kind: KeyKind) -> (IndexKey, u64) {
    let key = match kind {
        KeyKind::Null => IndexKey::Null,
        KeyKind::Bool => IndexKey::Bool(false),
        KeyKind::Number => IndexKey::Number(canonicalize_f64(f64::NEG_INFINITY), 0, i64::MIN),
        KeyKind::Str => IndexKey::Str(CompactString::default()),
        KeyKind::Uuid => IndexKey::Uuid(0, 0),
        KeyKind::Date => IndexKey::Date(i32::MIN),
        KeyKind::LocalDateTime => IndexKey::LocalDateTime(i64::MIN, 0),
        KeyKind::DateTime => IndexKey::DateTime(i64::MIN, 0),
        KeyKind::Time => IndexKey::Time(i64::MIN),
        // Every part at its minimum is the shortest length and the smallest fields.
        KeyKind::Duration => IndexKey::Duration(Duration::new(i32::MIN, i32::MIN, i64::MIN)),
        KeyKind::OffsetTime => IndexKey::OffsetTime(i64::MIN),
    };
    (key, 0)
}

/// The nanoseconds of a count above its whole microseconds (rounded down), `0..=999`.
fn sub_micros(nanos: i128) -> u16 {
    nanos.rem_euclid(1000) as u16
}

/// The bound just past a key kind: the first entry of the next kind, excluded.
fn kind_end(kind: KeyKind) -> Bound<(IndexKey, u64)> {
    match kind {
        KeyKind::Null => Bound::Excluded(kind_start(KeyKind::Bool)),
        KeyKind::Bool => Bound::Excluded(kind_start(KeyKind::Number)),
        KeyKind::Number => Bound::Excluded(kind_start(KeyKind::Str)),
        KeyKind::Str => Bound::Excluded(kind_start(KeyKind::Uuid)),
        KeyKind::Uuid => Bound::Excluded(kind_start(KeyKind::Date)),
        KeyKind::Date => Bound::Excluded(kind_start(KeyKind::LocalDateTime)),
        KeyKind::LocalDateTime => Bound::Excluded(kind_start(KeyKind::DateTime)),
        KeyKind::DateTime => Bound::Excluded(kind_start(KeyKind::Time)),
        KeyKind::Time => Bound::Excluded(kind_start(KeyKind::Duration)),
        KeyKind::Duration => Bound::Excluded(kind_start(KeyKind::OffsetTime)),
        KeyKind::OffsetTime => Bound::Unbounded,
    }
}

/// The set bounds of a range, `None` when the range holds nothing (bounds of different kinds,
/// a non-scalar bound, no bound at all, or a lower bound past the upper one).
type SetBounds = (Bound<(IndexKey, u64)>, Bound<(IndexKey, u64)>);

fn set_bounds(range: &PropertyIndexRange) -> Option<SetBounds> {
    let bound_key = |bound: &Bound<ElementProperty>| -> Option<Option<(IndexKey, bool)>> {
        match bound {
            Bound::Included(value) => IndexKey::of(value).map(|key| Some((key, true))),
            Bound::Excluded(value) => IndexKey::of(value).map(|key| Some((key, false))),
            Bound::Unbounded => Some(None),
        }
    };
    let lower = bound_key(&range.lower)?;
    let upper = bound_key(&range.upper)?;
    let kind = match (&lower, &upper) {
        (Some((a, _)), Some((b, _))) if a.kind() != b.kind() => return None,
        (Some((a, _)), _) => a.kind(),
        (None, Some((b, _))) => b.kind(),
        (None, None) => return None,
    };
    let start = match lower {
        None => Bound::Included(kind_start(kind)),
        // Numbers: every entry of the same float64 value, int64 and float64 alike (a superset;
        // the caller re-checks).
        Some((IndexKey::Number(value, _, _), _)) => {
            Bound::Included((IndexKey::Number(value, 0, i64::MIN), 0))
        }
        Some((key, true)) => Bound::Included((key, 0)),
        Some((key, false)) => Bound::Excluded((key, u64::MAX)),
    };
    let end = match upper {
        None => kind_end(kind),
        Some((IndexKey::Number(value, _, _), _)) => {
            Bound::Included((IndexKey::Number(value, 1, i64::MAX), u64::MAX))
        }
        Some((key, true)) => Bound::Included((key, u64::MAX)),
        Some((key, false)) => Bound::Excluded((key, 0)),
    };
    // `BTreeSet::range` panics on a start past the end: such a range holds nothing.
    let empty = match (&start, &end) {
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b))
            if a > b =>
        {
            true
        }
        (Bound::Excluded(a), Bound::Excluded(b)) if a == b => true,
        (Bound::Included(a), Bound::Excluded(b)) | (Bound::Excluded(a), Bound::Included(b))
            if a == b =>
        {
            true
        }
        _ => false,
    };
    (!empty).then_some((start, end))
}

// ---------------------------------------------------------------------------------------------
// The in-memory state
// ---------------------------------------------------------------------------------------------

/// A handle packed into one word: slot index in the high half, generation in the low half.
fn pack(slot: usize, generation: std::num::NonZeroU32) -> u64 {
    ((slot as u64) << 32) | u64::from(generation.get())
}

fn unpack(word: u64) -> Option<(usize, std::num::NonZeroU32)> {
    let generation = std::num::NonZeroU32::new(word as u32)?;
    Some(((word >> 32) as usize, generation))
}

/// One active index: its entries, ordered by key then handle.
#[derive(Debug, Clone)]
struct IndexState {
    name: CompactString,
    element: ElementKind,
    label: CompactString,
    key: CompactString,
    transform: Option<PropertyIndexTransform>,
    entries: BTreeSet<(IndexKey, u64)>,
    distinct: u64,
    heap: u64,
    /// Elements whose non-null value at the key the index does not hold.
    skipped: u64,
}

impl IndexState {
    /// Puts in (`insert`) or takes out the entry of a stored value of element `handle`: its key,
    /// or the `skipped` count for a non-null value the index does not hold.
    fn account(&mut self, insert: bool, value: &ElementProperty, handle: u64) {
        match transformed_key(self.transform.as_ref(), value) {
            Some(key) if insert => self.insert(key, handle),
            Some(key) => self.remove(key, handle),
            None if matches!(value, ElementProperty::Null) => {}
            None if insert => self.skipped += 1,
            None => self.skipped = self.skipped.saturating_sub(1),
        }
    }

    fn insert(&mut self, key: IndexKey, handle: u64) {
        let heap = key.heap_bytes();
        let fresh = !self.has_key(&key);
        if self.entries.insert((key, handle)) {
            self.heap += heap;
            if fresh {
                self.distinct += 1;
            }
        }
    }

    fn remove(&mut self, key: IndexKey, handle: u64) {
        let heap = key.heap_bytes();
        let entry = (key, handle);
        if self.entries.remove(&entry) {
            self.heap = self.heap.saturating_sub(heap);
            if !self.has_key(&entry.0) {
                self.distinct = self.distinct.saturating_sub(1);
            }
        }
    }

    /// Whether some entry has this key.
    fn has_key(&self, key: &IndexKey) -> bool {
        self.entries
            .range((
                Bound::Included((key.clone(), 0)),
                Bound::Included((key.clone(), u64::MAX)),
            ))
            .next()
            .is_some()
    }

    fn stats(&self) -> PropertyIndexStats {
        let entry = std::mem::size_of::<(IndexKey, u64)>() as u64 + 8;
        PropertyIndexStats {
            entries: self.entries.len() as u64,
            distinct_values: self.distinct,
            bytes: self.entries.len() as u64 * entry + self.heap,
            skipped: self.skipped,
        }
    }
}

/// The indexes of a graph (derived from the catalog), with lookups by element kind and label.
#[derive(Debug, Clone, Default)]
pub(crate) struct PropertyIndexes {
    indexes: Vec<IndexState>,
    vertex: HashMap<CompactString, Vec<usize>>,
    edge: HashMap<CompactString, Vec<usize>>,
}

impl PropertyIndexes {
    /// Whether no index exists at all.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }

    fn by_label(&self, element: ElementKind) -> &HashMap<CompactString, Vec<usize>> {
        match element {
            ElementKind::Vertex => &self.vertex,
            ElementKind::Edge => &self.edge,
        }
    }

    fn reindex_lookup(&mut self) {
        self.vertex.clear();
        self.edge.clear();
        for (position, index) in self.indexes.iter().enumerate() {
            let map = match index.element {
                ElementKind::Vertex => &mut self.vertex,
                ElementKind::Edge => &mut self.edge,
            };
            map.entry(index.label.clone()).or_default().push(position);
        }
    }

    /// Takes the entries of one element out (`insert` false) or puts them in (`insert` true):
    /// the element's labels and properties as stored; `only_key` limits it to one top-level key.
    fn element_entries<'l>(
        &mut self,
        element: ElementKind,
        insert: bool,
        handle: u64,
        labels: impl Iterator<Item = &'l str>,
        properties: Option<&PropertyMap>,
        only_key: Option<&str>,
    ) {
        let Some(properties) = properties else {
            return;
        };
        let by_label = match element {
            ElementKind::Vertex => &self.vertex,
            ElementKind::Edge => &self.edge,
        };
        for label in labels {
            let Some(positions) = by_label.get(label) else {
                continue;
            };
            for position in positions {
                let Some(index) = self.indexes.get_mut(*position) else {
                    continue;
                };
                if only_key.is_some_and(|key| key != index.key.as_str()) {
                    continue;
                }
                let Some(value) = properties.get(index.key.as_str()) else {
                    continue;
                };
                index.account(insert, value, handle);
            }
        }
    }

    /// [`Self::element_entries`] of a vertex (borrows only this state, so a caller holding the
    /// vertex's weight can use it).
    pub(crate) fn vertex_entries(
        &mut self,
        insert: bool,
        node: VertexHandle,
        labels: &[CompactString],
        properties: &PropertyMap,
        only_key: Option<&str>,
    ) {
        self.element_entries(
            ElementKind::Vertex,
            insert,
            pack(node.index().index(), node.generation()),
            labels.iter().map(CompactString::as_str),
            Some(properties),
            only_key,
        );
    }

    /// [`Self::element_entries`] of an edge.
    pub(crate) fn edge_entries(
        &mut self,
        insert: bool,
        edge: EdgeHandle,
        label: Option<&str>,
        properties: Option<&PropertyMap>,
        only_key: Option<&str>,
    ) {
        self.element_entries(
            ElementKind::Edge,
            insert,
            pack(edge.index().index(), edge.generation()),
            label.into_iter(),
            properties,
            only_key,
        );
    }

    /// The index on `(element, label, key, transform)`.
    fn find(
        &self,
        element: ElementKind,
        label: &str,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
    ) -> Option<&IndexState> {
        self.by_label(element)
            .get(label)?
            .iter()
            .filter_map(|position| self.indexes.get(*position))
            .find(|index| index.key == key && index.transform.as_ref() == transform)
    }

    /// The estimated heap footprint of every index, in bytes.
    pub(crate) fn bytes(&self) -> u64 {
        self.indexes.iter().map(|index| index.stats().bytes).sum()
    }
}

// ---------------------------------------------------------------------------------------------
// TraversalGraph: maintenance, rebuild, lookups, check
// ---------------------------------------------------------------------------------------------

impl TraversalGraph {
    /// Takes the index entries of vertex `node` out (`insert` false) or puts them back (`insert`
    /// true), for every index of its labels (`key`: only the indexes on that top-level key).
    /// Called around every mutation that can change what an index holds; one branch without
    /// indexes.
    #[inline]
    pub(crate) fn pindex_vertex(&mut self, node: VertexHandle, insert: bool, key: Option<&str>) {
        if self.property_indexes.is_empty() {
            return;
        }
        self.pindex_vertex_slow(node, insert, key);
    }

    #[inline(never)]
    fn pindex_vertex_slow(&mut self, node: VertexHandle, insert: bool, key: Option<&str>) {
        let Some(vertex) = self
            .graph
            .node_weight(node.index())
            .filter(|vertex| vertex.3 == node.generation())
        else {
            return;
        };
        self.property_indexes.vertex_entries(
            insert,
            node,
            self.labels.set(vertex.1),
            &vertex.2,
            key,
        );
    }

    /// The edge counterpart of [`Self::pindex_vertex`].
    #[inline]
    pub(crate) fn pindex_edge(&mut self, edge: EdgeHandle, insert: bool, key: Option<&str>) {
        if self.property_indexes.is_empty() {
            return;
        }
        self.pindex_edge_slow(edge, insert, key);
    }

    #[inline(never)]
    fn pindex_edge_slow(&mut self, edge: EdgeHandle, insert: bool, key: Option<&str>) {
        let Some(weight) = self
            .graph
            .edge_weight(edge.index())
            .filter(|weight| weight.3.own == edge.generation())
        else {
            return;
        };
        self.property_indexes.edge_entries(
            insert,
            edge,
            self.labels.name_str(weight.1),
            weight.2.as_ref(),
            key,
        );
    }

    /// Builds the state of one index from the data.
    fn build_index(&self, name: &str, definition: &PropertyIndexDefinition) -> IndexState {
        let mut state = IndexState {
            name: CompactString::from(name),
            element: definition.element(),
            label: CompactString::from(definition.label()),
            key: CompactString::from(definition.key()),
            transform: definition.transform().cloned(),
            entries: BTreeSet::new(),
            distinct: 0,
            heap: 0,
            skipped: 0,
        };
        match definition.element() {
            ElementKind::Vertex => {
                if let Some(nodes) = self.index.get_nodes_by_label(definition.label()) {
                    for node in nodes {
                        if let Some(vertex) = self.vertex_weight(node)
                            && let Some(value) = vertex.2.get(definition.key())
                        {
                            state.account(
                                true,
                                value,
                                pack(node.index().index(), node.generation()),
                            );
                        }
                    }
                }
            }
            ElementKind::Edge => {
                if let Some(edges) = self.index.get_edges_by_label(definition.label()) {
                    for edge in edges {
                        if let Some(weight) = self.edge_weight(edge)
                            && let Some(value) = weight
                                .2
                                .as_ref()
                                .and_then(|properties| properties.get(definition.key()))
                        {
                            state.account(
                                true,
                                value,
                                pack(edge.index().index(), edge.generation()),
                            );
                        }
                    }
                }
            }
        }
        state
    }

    /// After a catalog change of kind property index: rebuilds the index `name` from the data
    /// (or removes it when the catalog no longer holds it).
    pub(crate) fn property_index_after_change(&mut self, name: &str) {
        // An index of a type this version does not know is kept in the catalog, never built.
        let definition = self
            .definitions
            .get(DefinitionKind::PROPERTY_INDEX, name)
            .and_then(|definition| definition.as_property_index().cloned())
            .filter(PropertyIndexDefinition::is_served_type);
        let built = definition.map(|definition| self.build_index(name, &definition));
        let indexes = &mut self.property_indexes.indexes;
        match (indexes.iter().position(|index| index.name == name), built) {
            (Some(position), Some(state)) => indexes[position] = state,
            (Some(position), None) => {
                indexes.remove(position);
            }
            (None, Some(state)) => indexes.push(state),
            (None, None) => return,
        }
        self.property_indexes.reindex_lookup();
    }

    /// Rebuilds every index from the catalog and the data (a load: read-only opens, backups and
    /// recoveries are consistent with the data).
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn property_index_rebuild_all(&mut self) {
        let definitions: Vec<(String, PropertyIndexDefinition)> = self
            .definitions
            .iter()
            .filter_map(|definition| {
                definition
                    .as_property_index()
                    .filter(|index| index.is_served_type())
                    .map(|index| (definition.name().to_string(), index.clone()))
            })
            .collect();
        let indexes: Vec<IndexState> = definitions
            .iter()
            .map(|(name, definition)| self.build_index(name, definition))
            .collect();
        self.property_indexes = PropertyIndexes {
            indexes,
            ..PropertyIndexes::default()
        };
        self.property_indexes.reindex_lookup();
    }

    /// Every index this graph serves, with its stats, by catalog order.
    pub(crate) fn property_index_infos(&self) -> Vec<PropertyIndexInfo> {
        self.property_indexes
            .indexes
            .iter()
            .map(|index| PropertyIndexInfo {
                name: index.name.to_string(),
                element: index.element,
                label: index.label.to_string(),
                key: index.key.to_string(),
                index_type: PropertyIndexType::MemoryOrdered,
                transform: index.transform.clone(),
                stats: index.stats(),
            })
            .collect()
    }

    /// The entries of the index on `(element, label, key, transform)` inside `range`, as packed
    /// handles; `None` when there is no such index.
    fn index_slice<'a>(
        &'a self,
        element: ElementKind,
        label: &str,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = (usize, std::num::NonZeroU32)> + 'a>> {
        let index = self.property_indexes.find(element, label, key, transform)?;
        Some(match set_bounds(range) {
            Some(bounds) => Box::new(
                index
                    .entries
                    .range(bounds)
                    .filter_map(|(_, word)| unpack(*word)),
            ),
            None => Box::new(std::iter::empty()),
        })
    }

    /// The vertices labelled `label` whose `key` (transformed by `transform`) lies in `range` (a
    /// superset), from the index on `(vertex, label, key, transform)`; `None` without such an
    /// index.
    pub(crate) fn property_index_vertices<'a>(
        &'a self,
        label: &str,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = VertexHandle> + 'a>> {
        let slice = self.index_slice(ElementKind::Vertex, label, key, transform, range)?;
        Some(Box::new(slice.map(|(slot, generation)| {
            VertexHandle::new(NodeIndex::new(slot), generation)
        })))
    }

    /// The edge counterpart of [`Self::property_index_vertices`].
    pub(crate) fn property_index_edges<'a>(
        &'a self,
        label: &str,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = EdgeHandle> + 'a>> {
        let slice = self.index_slice(ElementKind::Edge, label, key, transform, range)?;
        Some(Box::new(slice.map(|(slot, generation)| {
            EdgeHandle::new(EdgeIndex::new(slot), generation)
        })))
    }

    /// Compares every maintained index with a fresh rebuild from the data and the catalog:
    /// `Err` names the first index that differs. A test helper; it walks every index's label.
    #[doc(hidden)]
    pub fn check_property_indexes(&self) -> Result<(), String> {
        let mut expected: Vec<(String, PropertyIndexDefinition)> = self
            .definitions
            .iter()
            .filter_map(|definition| {
                definition
                    .as_property_index()
                    .filter(|index| index.is_served_type())
                    .map(|index| (definition.name().to_string(), index.clone()))
            })
            .collect();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        let mut actual: Vec<&IndexState> = self.property_indexes.indexes.iter().collect();
        actual.sort_by(|a, b| a.name.cmp(&b.name));
        if expected.len() != actual.len() {
            return Err(format!(
                "{} indexes maintained, the catalog declares {}",
                actual.len(),
                expected.len()
            ));
        }
        for ((name, definition), state) in expected.iter().zip(actual) {
            let fresh = self.build_index(name, definition);
            if state.name != fresh.name
                || state.element != fresh.element
                || state.label != fresh.label
                || state.key != fresh.key
                || state.transform != fresh.transform
            {
                return Err(format!("index '{name}': its definition differs"));
            }
            if state.entries != fresh.entries {
                return Err(format!(
                    "index '{name}': {} entries maintained, {} after a rebuild (or other values)",
                    state.entries.len(),
                    fresh.entries.len()
                ));
            }
            if state.skipped != fresh.skipped {
                return Err(format!(
                    "index '{name}': {} skipped values counted, {} after a rebuild",
                    state.skipped, fresh.skipped
                ));
            }
            if state.distinct != fresh.distinct {
                return Err(format!(
                    "index '{name}': {} distinct values counted, {} after a rebuild",
                    state.distinct, fresh.distinct
                ));
            }
        }
        // The lookup tables match the indexes.
        let mut lookup = self.property_indexes.clone();
        lookup.reindex_lookup();
        if lookup.vertex != self.property_indexes.vertex
            || lookup.edge != self.property_indexes.edge
        {
            return Err("the label lookup of the indexes is stale".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphStorage;

    fn int(n: i64) -> ElementProperty {
        ElementProperty::from(n)
    }

    #[test]
    fn keys_order_kinds_and_numbers() {
        let one = IndexKey::of(&int(1)).unwrap();
        let one_f = IndexKey::of(&ElementProperty::from(1.0)).unwrap();
        let two = IndexKey::of(&int(2)).unwrap();
        assert!(one < one_f && one_f < two);
        assert!(IndexKey::of(&ElementProperty::Null).unwrap() < one);
        assert!(two < IndexKey::of(&ElementProperty::from("a")).unwrap());
        assert!(IndexKey::of(&ElementProperty::Array(vec![])).is_none());
    }

    #[test]
    fn temporal_keys_are_one_kind_per_type_and_follow_has() {
        let key = |text: &str, kind: crate::value::temporal::TemporalKind| {
            IndexKey::of(&ElementProperty::Temporal(
                Temporal::parse_as(kind, text).unwrap(),
            ))
            .unwrap()
        };
        use crate::value::temporal::TemporalKind as K;
        let noon = key("2023-08-02T12:00:00Z", K::DateTime);
        assert_eq!(noon, key("2023-08-02T14:00:00+02:00", K::DateTime));
        assert_ne!(key("P1M", K::Duration), key("P30D", K::Duration));
        assert!(key("PT720H", K::Duration) < key("P30D", K::Duration));
        assert!(key("P30D", K::Duration) < key("P1M", K::Duration));
        assert!(key("P1M", K::Duration) < key("P30DT1S", K::Duration));
        // Kinds in order, each inside its own slice.
        let kinds = [
            key("9999-12-31", K::Date),
            key("-9999-01-01T00:00:00", K::LocalDateTime),
            noon,
            key("00:00:00", K::Time),
            key("P-100Y", K::Duration),
        ];
        assert!(IndexKey::Uuid(u64::MAX, u64::MAX) < kinds[0]);
        for pair in kinds.windows(2) {
            assert!(pair[0] < pair[1]);
            assert_ne!(pair[0].kind(), pair[1].kind());
        }
        for key in &kinds {
            let start = kind_start(key.kind());
            assert!(start <= (key.clone(), 0));
            match kind_end(key.kind()) {
                Bound::Excluded(end) => assert!((key.clone(), u64::MAX) < end),
                _ => assert_eq!(key.kind(), KeyKind::Duration),
            }
        }
    }

    #[test]
    fn index_keys_follow_the_type_matrix() {
        use crate::graph::schema::type_of;
        use crate::value::temporal::TemporalKind as K;
        let temporal = |kind: K, text: &str| {
            ElementProperty::Temporal(Temporal::parse_as(kind, text).unwrap())
        };
        let mut object = PropertyMap::default();
        object.insert("a", int(1));
        let values = [
            ElementProperty::Null,
            ElementProperty::Boolean(true),
            int(1),
            ElementProperty::from(1.5),
            ElementProperty::from("text"),
            ElementProperty::Uuid(7),
            temporal(K::Date, "2024-01-02"),
            temporal(K::LocalDateTime, "2024-01-02T03:04:05"),
            temporal(K::DateTime, "2024-01-02T03:04:05Z"),
            temporal(K::Time, "03:04:05"),
            temporal(K::Duration, "P1D"),
            ElementProperty::Array(vec![int(1)]),
            ElementProperty::Object(Box::new(object)),
            ElementProperty::Vector(crate::value::Vector::from_f32(vec![1.0, 2.0]).unwrap()),
        ];
        for value in &values {
            let ty = type_of(value).unwrap();
            assert_eq!(
                is_indexable(value),
                PropertyIndexType::MemoryOrdered.holds(&ty),
                "{ty}"
            );
        }
        assert!(!PropertyIndexType::from_name("x").holds(&ValueType::Int64));
        assert!(
            PropertyIndexType::MemoryOrdered.holds(&ValueType::Union(vec![
                ValueType::Object,
                ValueType::String
            ]))
        );
    }

    #[test]
    fn inverted_and_mixed_ranges_hold_nothing() {
        let range = PropertyIndexRange::new(Bound::Included(int(5)), Bound::Excluded(int(1)));
        assert!(set_bounds(&range).is_none());
        let range = PropertyIndexRange::new(Bound::Included(int(5)), Bound::Excluded("a".into()));
        assert!(set_bounds(&range).is_none());
        let range = PropertyIndexRange::new(Bound::Excluded(int(5)), Bound::Excluded(int(5)));
        assert!(
            set_bounds(&range).is_some(),
            "numbers are widened to their float value"
        );
        let range =
            PropertyIndexRange::new(Bound::Excluded("a".into()), Bound::Excluded("a".into()));
        assert!(set_bounds(&range).is_none());
    }

    #[test]
    fn maintained_through_writes_and_rollback() {
        let mut graph = TraversalGraph::new();
        let mut props = PropertyMap::default();
        props.insert("age", int(29));
        let v = graph
            .insert_vertex(None, vec!["person".into()], props)
            .unwrap();
        graph
            .set_definition(Definition::property_index(
                "age",
                PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
            ))
            .unwrap();
        graph.check_property_indexes().unwrap();
        let found: Vec<_> = graph
            .property_index_vertices("person", "age", None, &PropertyIndexRange::equal(int(29)))
            .unwrap()
            .collect();
        assert_eq!(found, vec![v]);
        let mark = graph.begin_unit();
        graph.set_vertex_property(v, "age".into(), int(30)).unwrap();
        graph.check_property_indexes().unwrap();
        graph.rollback_unit(mark, &crate::graph::RollbackReason::failed(&"test"));
        graph.check_property_indexes().unwrap();
        graph.drop_vertex(v).unwrap();
        graph.check_property_indexes().unwrap();
    }
}
