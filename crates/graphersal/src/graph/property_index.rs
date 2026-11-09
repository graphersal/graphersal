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
//! by an index (an index with the transform `each` holds the scalar ELEMENTS of an array, below). The index is ordered: an equality, `P.within` and the ranges `P.gt/gte/lt/lte/
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
//! compared AS GIVEN with the lowered values. It also knows `each` (one key per element of an
//! array value, a scalar value as itself; it serves `has(key, __.unfold().is(P))`, and a
//! condition on the whole value as a superset) and the chain `["each", "to_lower"]` (payload: an
//! array of names). An index with `each` is stored as [`DefinitionKind::PROPERTY_INDEX_EXT`]
//! (critical), also per label, and its lookups deduplicate an element held under several keys
//! of a slice. An unknown transform (or a chain in an order this version does not serve) is
//! handled like an unknown type: kept byte for byte, never built or served, never created.
//!
//! LAYOUT: an index keeps each key once with its POSTINGS, the sorted slots of the elements
//! holding it (`BTreeMap<IndexKey, Postings>`, `graph/postings.rs`): one slot inline in the
//! 8-byte map value, from the second on a sorted run in the index's `RunsStore`, sorted chunks
//! past 4096 slots. A slot carries no generation (exact
//! maintenance: a slot an index names belongs to one live element); a lookup reads the
//! generation from the slot. Lookups go by key, then by slot.

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;

use compact_str::CompactString;

use crate::error::{DefinitionError, GraphError};
use crate::graph::definition::{Definition, DefinitionBody, DefinitionFlags, DefinitionKind};
use crate::graph::postings::{Postings, Removed, RunsStore};
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
/// "Property Indexes", sections "Case-insensitive search" and "Arrays: the each transform"):
/// the index keys are the transformed values, and the index serves a condition that applies the
/// same transform before its predicate.
///
/// This version knows two transforms, [`Self::ToLower`] (named after the step `to_lower()`) and
/// [`Self::Each`] (named after "each element", the step `unfold()`), and one CHAIN of them,
/// `each` then `to_lower` ([`Self::chain`], [`Self::Chain`]; the payload stores a single
/// transform as its name, a chain as the array of its names, `["each", "to_lower"]`). A
/// transform name this version does not know (a definition written by a newer version) is
/// [`Self::Unknown`], and so is a chain holding one; a chain of known names in an order this
/// version does not serve (`["to_lower", "each"]`) is not [known](Self::is_known) either: the
/// definition is kept and written back unchanged, but the index is never built or served, and
/// creating one is refused ([`PropertyIndexDefinition::check_create`]).
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
///
/// let chain = PropertyIndexTransform::from_names(["each", "to_lower"]).unwrap();
/// assert_eq!(chain.as_str(), "each to_lower");
/// assert_eq!(chain.names(), vec!["each", "to_lower"]);
/// assert!(chain.is_known() && chain.has_each());
/// let wrong = PropertyIndexTransform::from_names(["to_lower", "each"]).unwrap();
/// assert!(!wrong.is_known());
/// // A chain of one transform is that transform.
/// assert_eq!(
///     PropertyIndexTransform::from_names(["each"]),
///     Some(PropertyIndexTransform::Each)
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PropertyIndexTransform {
    /// `"to_lower"`: the key is the lowered text of a STRING value (a compressed one by its
    /// text), computed exactly as the step `to_lower()` computes it (Rust's `str::to_lowercase`:
    /// Unicode, no locale); a value of any other type is not in the index. Serves
    /// `has(key, __.to_lower().is(P))` with `P` an equality, `P.within` or a range.
    ToLower,
    /// `"each"`: an ARRAY value gives one key per element (an element held twice gives one
    /// entry; a nested array, an object or a vector inside it is not a key and counts in
    /// [`skipped`](PropertyIndexStats::skipped)), any other value its one key, as the step
    /// `unfold()` emits them. Serves `has(key, __.unfold().is(P))` exactly (arrays are one
    /// value, design D8: there are no multi-properties), and `has(key, value)` /
    /// `has(key, P)` as a superset (the source re-checks the whole value), with `P` an
    /// equality, `P.within` or a range.
    Each,
    /// A chain of transforms applied in order (built by [`Self::chain`], never of fewer than two
    /// transforms, never nested): this version serves `each` then `to_lower`, the lowered text
    /// of every string element, for `has(key, __.unfold().to_lower().is(P))`.
    Chain(PropertyIndexTransformChain),
    /// A transform name this version does not know, as read: kept, never served, never created.
    Unknown(String),
}

/// The transforms of a [`PropertyIndexTransform::Chain`], in the order they apply, with the
/// chain's name (the names joined by a space, as `.profile()` renders it). Built only by
/// [`PropertyIndexTransform::chain`], so it always holds two or more transforms, none a chain.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PropertyIndexTransformChain {
    steps: Vec<PropertyIndexTransform>,
    name: String,
}

impl PropertyIndexTransformChain {
    /// The transforms of the chain, in the order they apply.
    pub fn steps(&self) -> &[PropertyIndexTransform] {
        &self.steps
    }
}

impl PropertyIndexTransform {
    /// The name of [`Self::ToLower`].
    pub const TO_LOWER: &'static str = "to_lower";

    /// The name of [`Self::Each`].
    pub const EACH: &'static str = "each";

    /// The names of every transform this version knows, in the order front ends offer them
    /// (a chain lists them in the order they apply, `each` first).
    pub const KNOWN_NAMES: [&'static str; 2] = [Self::TO_LOWER, Self::EACH];

    /// The transform of a name: a known one, else [`Self::Unknown`] with the name (never fails).
    pub fn from_name(name: &str) -> PropertyIndexTransform {
        match name {
            Self::TO_LOWER => PropertyIndexTransform::ToLower,
            Self::EACH => PropertyIndexTransform::Each,
            other => PropertyIndexTransform::Unknown(other.to_string()),
        }
    }

    /// The transform of a list of names applied in order ([`Self::chain`] of
    /// [`Self::from_name`]s); `None` for an empty list.
    pub fn from_names<I, N>(names: I) -> Option<PropertyIndexTransform>
    where
        I: IntoIterator<Item = N>,
        N: AsRef<str>,
    {
        Self::chain(names.into_iter().map(|name| Self::from_name(name.as_ref())))
    }

    /// The chain of `steps` applied in order: one transform is that transform, two or more a
    /// [`Self::Chain`] (a chain inside is flattened); `None` for none. Any order is built; only
    /// the orders this version serves are [known](Self::is_known) (`each` first).
    pub fn chain(
        steps: impl IntoIterator<Item = PropertyIndexTransform>,
    ) -> Option<PropertyIndexTransform> {
        let mut flat = Vec::new();
        for step in steps {
            match step {
                PropertyIndexTransform::Chain(chain) => flat.extend(chain.steps),
                other => flat.push(other),
            }
        }
        match flat.len() {
            0 => None,
            1 => flat.pop(),
            _ => {
                let name = flat
                    .iter()
                    .map(PropertyIndexTransform::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(PropertyIndexTransform::Chain(PropertyIndexTransformChain {
                    steps: flat,
                    name,
                }))
            }
        }
    }

    /// The transform's name: `"to_lower"`, `"each"`, the unknown name as read, or for a chain
    /// its names joined by a space (`"each to_lower"`, as `.profile()` renders it; the payload
    /// stores a chain as the array of [`Self::names`]).
    pub fn as_str(&self) -> &str {
        match self {
            PropertyIndexTransform::ToLower => Self::TO_LOWER,
            PropertyIndexTransform::Each => Self::EACH,
            PropertyIndexTransform::Chain(chain) => &chain.name,
            PropertyIndexTransform::Unknown(name) => name,
        }
    }

    /// The transforms applied in order: the chain's, or this one alone.
    pub fn steps(&self) -> &[PropertyIndexTransform] {
        match self {
            PropertyIndexTransform::Chain(chain) => &chain.steps,
            PropertyIndexTransform::ToLower
            | PropertyIndexTransform::Each
            | PropertyIndexTransform::Unknown(_) => std::slice::from_ref(self),
        }
    }

    /// The names of [`Self::steps`], as the payload stores a chain.
    pub fn names(&self) -> Vec<&str> {
        self.steps()
            .iter()
            .map(PropertyIndexTransform::as_str)
            .collect()
    }

    /// Whether the transform starts with [`Self::Each`] (one key per array element).
    pub fn has_each(&self) -> bool {
        self.steps().first() == Some(&PropertyIndexTransform::Each)
    }

    /// Whether a step of the transform can FAIL on a value the index skips (`to_lower()` on a
    /// non-string): an index of such a transform serves only while it skipped no value.
    pub(crate) fn can_fail(&self) -> bool {
        self.steps()
            .iter()
            .any(|step| *step != PropertyIndexTransform::Each)
    }

    /// What is applied to each element after [`Self::Each`] (`None`: nothing), for a transform
    /// that [has it](Self::has_each).
    fn after_each(&self) -> Option<&PropertyIndexTransform> {
        match self {
            PropertyIndexTransform::Chain(chain) => chain.steps.get(1),
            PropertyIndexTransform::ToLower
            | PropertyIndexTransform::Each
            | PropertyIndexTransform::Unknown(_) => None,
        }
    }

    /// Why a chain of known transforms is not one this version serves, `None` when it is (or
    /// holds an unknown name, reported as such).
    fn order_error(&self) -> Option<String> {
        let PropertyIndexTransform::Chain(chain) = self else {
            return None;
        };
        if chain.steps.iter().any(|step| !step.is_single_known()) {
            return None;
        }
        let served = matches!(
            chain.steps.as_slice(),
            [
                PropertyIndexTransform::Each,
                PropertyIndexTransform::ToLower
            ]
        );
        (!served).then(|| {
            format!(
                "the chain [{}] is not one this version serves: \"each\" comes first and \
                 every transform at most once (known: [\"each\", \"to_lower\"])",
                chain
                    .steps
                    .iter()
                    .map(|step| format!("\"{step}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }

    /// Whether this is one transform this version knows (not a chain).
    fn is_single_known(&self) -> bool {
        match self {
            PropertyIndexTransform::ToLower | PropertyIndexTransform::Each => true,
            PropertyIndexTransform::Chain(_) | PropertyIndexTransform::Unknown(_) => false,
        }
    }

    /// Whether this version knows the transform (and so can build and serve the index): a
    /// known name, or a chain of known names in an order this version serves.
    pub fn is_known(&self) -> bool {
        match self {
            PropertyIndexTransform::ToLower | PropertyIndexTransform::Each => true,
            PropertyIndexTransform::Chain(chain) => {
                chain.steps.iter().all(Self::is_single_known) && self.order_error().is_none()
            }
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
            PropertyIndexTransform::Each => Some(
                "Indexes every element of an array value (any other value as itself), for \
                 \"arrays containing x\": query it with has(key, __.unfold().is(..)).",
            ),
            PropertyIndexTransform::Chain(_) if self.is_known() => Some(
                "Indexes the lowered text of every string element of an array value, for a \
                 case-insensitive \"arrays containing x\": query it with \
                 has(key, __.unfold().to_lower().is(..)).",
            ),
            PropertyIndexTransform::Chain(_) | PropertyIndexTransform::Unknown(_) => None,
        }
    }

    /// Whether the transform takes values of type `ty` (a union when one of its members is):
    /// [`Self::ToLower`] takes strings only; [`Self::Each`] takes every type (an array by its
    /// elements); a chain what its first transform takes; an unknown transform takes nothing.
    /// [`PropertyIndexDefinition::holds`] checks an array's item types for an `each` index.
    pub fn accepts(&self, ty: &ValueType) -> bool {
        match ty {
            ValueType::Union(members) => members.iter().any(|member| self.accepts(member)),
            _ => match self {
                PropertyIndexTransform::ToLower => *ty == ValueType::String,
                PropertyIndexTransform::Each => true,
                PropertyIndexTransform::Chain(_) => {
                    self.is_known() && self.steps().first().is_some_and(|step| step.accepts(ty))
                }
                PropertyIndexTransform::Unknown(_) => false,
            },
        }
    }

    /// The transformed value of a stored value, `None` when the transform does not take it
    /// (the element is then not in the index). One value only: an `each` transform's keys come
    /// from [`value_keys`].
    fn apply(&self, value: &ElementProperty) -> Option<ElementProperty> {
        match self {
            PropertyIndexTransform::ToLower => value.text().map(|text| {
                ElementProperty::String(
                    crate::traversal::step::map_steps::step_to_lower::lower_text(&text).into(),
                )
            }),
            PropertyIndexTransform::Each
            | PropertyIndexTransform::Chain(_)
            | PropertyIndexTransform::Unknown(_) => None,
        }
    }
}

impl std::fmt::Display for PropertyIndexTransform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The payload value of a transform: its name, or for a chain the array of its names
/// (`["each", "to_lower"]`).
pub(crate) fn transform_payload(transform: &PropertyIndexTransform) -> ElementProperty {
    match transform {
        PropertyIndexTransform::Chain(chain) => ElementProperty::Array(
            chain
                .steps()
                .iter()
                .map(|step| ElementProperty::from(step.as_str()))
                .collect(),
        ),
        PropertyIndexTransform::ToLower
        | PropertyIndexTransform::Each
        | PropertyIndexTransform::Unknown(_) => ElementProperty::from(transform.as_str()),
    }
}

/// The index key of ONE stored value under an optional transform without `each`; `None` when
/// the index does not hold the value.
fn transformed_key(
    transform: Option<&PropertyIndexTransform>,
    value: &ElementProperty,
) -> Option<IndexKey> {
    match transform {
        None => IndexKey::of(value),
        Some(transform) => IndexKey::of(&transform.apply(value)?),
    }
}

/// A value as an index without `each` keys it (the lowered text for `to_lower`), for a
/// report; the value itself when the transform does not take it.
fn keyed_value(
    transform: Option<&PropertyIndexTransform>,
    value: &ElementProperty,
) -> ElementProperty {
    transform
        .and_then(|transform| transform.apply(value))
        .unwrap_or_else(|| value.clone())
}

/// The keys of a stored value under an optional transform, each passed to `key` (an `each`
/// transform passes one per array element, the same key again for an element held twice: the
/// postings keep a slot once per key); returns how many values the index does not hold (the
/// [`skipped`](PropertyIndexStats::skipped) count: a non-null value without a key; with `each`
/// one per such element of an array).
fn value_keys(
    transform: Option<&PropertyIndexTransform>,
    value: &ElementProperty,
    mut key: impl FnMut(IndexKey),
) -> u64 {
    let mut one = |transform: Option<&PropertyIndexTransform>, value: &ElementProperty| match (
        transformed_key(transform, value),
        value,
    ) {
        (Some(found), _) => {
            key(found);
            0
        }
        (None, ElementProperty::Null) => 0,
        (None, _) => 1,
    };
    match transform {
        Some(transform) if transform.has_each() => {
            let rest = transform.after_each();
            match value {
                ElementProperty::Array(items) => items.iter().map(|item| one(rest, item)).sum(),
                other => one(rest, other),
            }
        }
        other => one(other, value),
    }
}

/// Whether a stored value lies in one of `ranges` as the index of `transform` keys it (with
/// `each` an array by any of its elements): exactly the elements a lookup of those slices
/// names. The check of an exact index access's condition when a scan replaces the index
/// (`IndexAccess::holds`).
pub(crate) fn value_in_ranges(
    transform: Option<&PropertyIndexTransform>,
    value: &ElementProperty,
    ranges: &[PropertyIndexRange],
) -> bool {
    use std::ops::RangeBounds;
    let bounds: Vec<(Bound<IndexKey>, Bound<IndexKey>)> = ranges
        .iter()
        .filter_map(|range| set_bounds(range).and_then(key_bounds))
        .collect();
    let mut found = false;
    value_keys(transform, value, |key| {
        found = found || bounds.iter().any(|bounds| bounds.contains(&key));
    });
    found
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
                    // An array of the item types its `items` declares (an empty union: no
                    // declaration), which an `each` index checks.
                    JsonType::Array => match &node.vector {
                        Some(vector) => ValueType::Vector(vector.element),
                        None => ValueType::Array(Box::new(ValueType::Union(
                            node.items
                                .as_deref()
                                .and_then(declared_value_types)
                                .unwrap_or_default(),
                        ))),
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

/// A property index (catalog kind [`DefinitionKind::PROPERTY_INDEX`], or
/// [`DefinitionKind::PROPERTY_INDEX_EXT`] for a label-less one): the `element`s labelled
/// `label` (every element of the kind for a [label-less](Self::label_less) index), ordered by
/// the value of their top-level property `key`, in an index of type
/// [`index_type`](Self::index_type) (default [`PropertyIndexType::MemoryOrdered`]).
///
/// ```
/// use graphersal::catalog::{PropertyIndexDefinition, PropertyIndexType};
/// use graphersal::storage::ElementKind;
///
/// let index = PropertyIndexDefinition::new(ElementKind::Vertex, "person", "title");
/// assert_eq!(index.label(), Some("person"));
/// assert_eq!(index.key(), "title");
/// assert_eq!(index.index_type(), &PropertyIndexType::MemoryOrdered);
/// let any = PropertyIndexDefinition::label_less(ElementKind::Vertex, "name");
/// assert_eq!(any.label(), None);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyIndexDefinition {
    element: ElementKind,
    /// `None`: a label-less index (every element of the kind).
    label: Option<String>,
    key: String,
    index_type: PropertyIndexType,
    transform: Option<PropertyIndexTransform>,
    /// A unique constraint (book page "Property Indexes", section "Unique indexes"): no two
    /// elements of the index hold an equal key (`null` and values the index does not hold are
    /// exempt).
    unique: bool,
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
            label: Some(label.into()),
            key: key.into(),
            index_type: PropertyIndexType::MemoryOrdered,
            transform: None,
            unique: false,
            extra: PropertyMap::default(),
            original: None,
            unknown_element: None,
        }
    }

    /// A LABEL-LESS index (book page "Property Indexes", section "Label-less indexes"): every
    /// element of kind `element`, unlabeled ones included, on its top-level property `key`. It
    /// serves a `v()`/`e()` source without labels (`g.v().has("name", x)`), and a labelled
    /// source that has no per-label index on the key (the source then checks each candidate's
    /// labels). Stored as [`DefinitionKind::PROPERTY_INDEX_EXT`] with the critical flag
    /// ([`Self::needs_extended_kind`]).
    ///
    /// ```
    /// use graphersal::catalog::{Definition, DefinitionKind, PropertyIndexDefinition};
    /// use graphersal::storage::ElementKind;
    ///
    /// let index = PropertyIndexDefinition::label_less(ElementKind::Edge, "_owner");
    /// assert!(index.needs_extended_kind());
    /// assert_eq!(index.default_name(), "index__owner_edge");
    /// let definition = Definition::property_index("owner", index);
    /// assert_eq!(definition.kind(), DefinitionKind::PROPERTY_INDEX_EXT);
    /// assert!(definition.flags().is_critical());
    /// ```
    pub fn label_less(element: ElementKind, key: impl Into<String>) -> Self {
        PropertyIndexDefinition {
            element,
            label: None,
            key: key.into(),
            index_type: PropertyIndexType::MemoryOrdered,
            transform: None,
            unique: false,
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

    /// The same index with (`true`) or without a UNIQUE constraint (book page "Property
    /// Indexes", section "Unique indexes"): no two elements of the index hold an equal key, so
    /// a write that would give another element a key the index already holds fails with
    /// [`GraphError::UniqueViolation`] (checked at once, at every write). A per-label index is
    /// unique within its label, a [label-less](Self::label_less) one over the whole element
    /// kind, one with `to_lower` case-insensitively. `null` and values the index does not hold
    /// ([`skipped`](PropertyIndexStats::skipped)) are not part of uniqueness. A unique index is
    /// stored as [`DefinitionKind::PROPERTY_INDEX_EXT`] (critical); with the transform `each`
    /// it is refused at create ([`Self::check_create`]) and never served.
    ///
    /// ```
    /// use graphersal::catalog::{Definition, DefinitionKind, PropertyIndexDefinition};
    /// use graphersal::storage::ElementKind;
    ///
    /// let email = PropertyIndexDefinition::new(ElementKind::Vertex, "User", "email")
    ///     .with_unique(true);
    /// assert!(email.is_unique() && email.needs_extended_kind());
    /// let definition = Definition::property_index("user_email", email);
    /// assert_eq!(definition.kind(), DefinitionKind::PROPERTY_INDEX_EXT);
    /// ```
    pub fn with_unique(mut self, unique: bool) -> Self {
        // An index of an unknown element kind is kept as read.
        if self.unknown_element.is_none() {
            self.unique = unique;
            self.original = None;
        }
        self
    }

    /// Whether the index is a unique constraint ([`Self::with_unique`]).
    pub fn is_unique(&self) -> bool {
        self.unique
    }

    /// Why a unique index of this transform cannot be built (design D9: `each` holds several
    /// keys per element), `None` when it can.
    fn unique_error(&self) -> Option<&'static str> {
        (self.unique
            && self
                .transform
                .as_ref()
                .is_some_and(PropertyIndexTransform::has_each))
        .then_some(
            "a unique index cannot use the transform each (an array gives one key per element; \
             uniqueness of array elements is not supported)",
        )
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
        let transform = self.transform.as_ref();
        if let Some(transform) = transform.filter(|transform| transform.has_each()) {
            // `each`: an array by its item types (an array of items of no declared type is
            // held), any other value as itself; what follows `each` applies to each of them.
            let rest = transform.after_each();
            let element = |ty: &ValueType| {
                self.index_type.holds(ty) && rest.is_none_or(|rest| rest.accepts(ty))
            };
            return transform.is_known()
                && match ty {
                    ValueType::Union(members) => members.iter().any(|member| self.holds(member)),
                    ValueType::Array(item) => match item.as_ref() {
                        ValueType::Union(members) if members.is_empty() => true,
                        ValueType::Union(members) => members.iter().any(element),
                        item => element(item),
                    },
                    other => element(other),
                };
        }
        match ty {
            ValueType::Union(members) => members.iter().any(|member| self.holds(member)),
            _ => {
                self.index_type.holds(ty) && transform.is_none_or(|transform| transform.accepts(ty))
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
            Some(transform) if !transform.is_known() => Some(match transform.order_error() {
                Some(reason) => format!("{reason}; the definition is kept, queries scan"),
                None => format!(
                    "index transform '{transform}' is not known to this version (a newer \
                     Graphersal wrote it); the definition is kept, queries scan"
                ),
            }),
            Some(_) | None => self
                .unique_error()
                .map(|reason| format!("{reason}; the definition is kept, queries scan")),
        }
    }

    /// Whether this version builds and serves the index (its type and transform are known,
    /// and a unique one has no `each`).
    pub(crate) fn is_served_type(&self) -> bool {
        self.unknown_element.is_none()
            && self.index_type.is_known()
            && self
                .transform
                .as_ref()
                .is_none_or(PropertyIndexTransform::is_known)
            && self.unique_error().is_none()
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
    ///   [`DefinitionError::IndexTypeMismatch`]. A label-less index checks the key in EVERY
    ///   label of its element kind that declares it (the error names the first such label
    ///   whose declaration the index cannot hold). A declaration with at least one held
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
        if let Some(transform) = &self.transform
            && !transform.is_known()
        {
            return Err(DefinitionError::InvalidField {
                name: name.to_string(),
                field: "transform".to_string(),
                reason: match transform.order_error() {
                    Some(reason) => reason,
                    None => format!(
                        "'{}' is not an index transform this version knows (known: {})",
                        transform
                            .steps()
                            .iter()
                            .find(|step| !step.is_single_known())
                            .unwrap_or(transform),
                        PropertyIndexTransform::KNOWN_NAMES.join(", ")
                    ),
                },
            });
        }
        if let Some(reason) = self.unique_error() {
            return Err(DefinitionError::InvalidField {
                name: name.to_string(),
                field: "unique".to_string(),
                reason: reason.to_string(),
            });
        }
        let Some(schema) = schema else {
            return Ok(());
        };
        // A label-less index checks the key in every label of its kind that declares it.
        let labels: Vec<(&str, &PropertySchema)> = match (self.element, &self.label) {
            (ElementKind::Vertex, Some(label)) => schema
                .vertices
                .get(label)
                .map(|v| (label.as_str(), &v.schema))
                .into_iter()
                .collect(),
            (ElementKind::Edge, Some(label)) => schema
                .edges
                .get(label)
                .map(|e| (label.as_str(), &e.schema))
                .into_iter()
                .collect(),
            (ElementKind::Vertex, None) => schema
                .vertices
                .iter()
                .map(|(label, v)| (label.as_str(), &v.schema))
                .collect(),
            (ElementKind::Edge, None) => schema
                .edges
                .iter()
                .map(|(label, e)| (label.as_str(), &e.schema))
                .collect(),
        };
        for (label, root) in labels {
            let Some(node) = root.properties.get(&self.key) else {
                continue;
            };
            let Some(types) = declared_value_types(node) else {
                continue;
            };
            let mut non_null = types.iter().filter(|ty| **ty != ValueType::Null).peekable();
            if non_null.peek().is_none() || non_null.any(|ty| self.holds(ty)) {
                continue;
            }
            return Err(DefinitionError::IndexTypeMismatch {
                element: self.element,
                label: label.to_string(),
                key: self.key.clone(),
                declared: node.non_null_type_name(),
                index_type: self.index_type.as_str().into(),
                transform: self.transform.as_ref().map(|t| t.as_str().into()),
            });
        }
        Ok(())
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

    /// The label the index covers (it need not exist yet); `None` for a
    /// [label-less](Self::label_less) index, which covers every element of its kind.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Whether the index is [label-less](Self::label_less).
    pub fn is_label_less(&self) -> bool {
        self.label.is_none()
    }

    /// Whether the index uses a feature a reader must know to keep it exact, so it is stored
    /// as [`DefinitionKind::PROPERTY_INDEX_EXT`] with the critical flag (a reader that does not
    /// know that kind opens the store read-only): today a [label-less](Self::label_less) index,
    /// a [unique](Self::with_unique) one (an older writer would not enforce it) and an index
    /// whose transform [starts with `each`](PropertyIndexTransform::has_each) (per label too). A plain per-label index, also with `to_lower`, is
    /// [`DefinitionKind::PROPERTY_INDEX`], not critical.
    pub fn needs_extended_kind(&self) -> bool {
        self.label.is_none()
            || self.unique
            || self
                .transform
                .as_ref()
                .is_some_and(PropertyIndexTransform::has_each)
    }

    /// The catalog kind [`Definition::property_index`] stores this index as.
    pub(crate) fn definition_kind(&self) -> DefinitionKind {
        if self.needs_extended_kind() {
            DefinitionKind::PROPERTY_INDEX_EXT
        } else {
            DefinitionKind::PROPERTY_INDEX
        }
    }

    /// The top-level property key the index orders by.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The name an index gets when none is given (the DSL's `g.create_index` without `name`,
    /// the session, MCP, the playground's form): `<label>_<key>` (`<key>` for a label-less
    /// index; `<label>_<key>_edge` for an edge index), then `_<transform>` for an index with a
    /// transform (`person_name_to_lower`, `knows_note_edge_to_lower`; a chain by its names
    /// joined by `_`: `index__owners_each`, `person_tags_each_to_lower`), every character that is
    /// not an ASCII letter, digit or `_` replaced by `_`, prefixed with `index_` when it would
    /// not start with a letter (`index__owner_edge` for a label-less edge index on `_owner`).
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
        let base = match &self.label {
            Some(label) => format!("{label}_{}", self.key),
            None => self.key.clone(),
        };
        let mut name: String = base
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
    /// `"edge"`), `label` (absent for a label-less index), `key`, `type` (always written),
    /// `transform` (only for an index with a transform: its name, or a chain as the array of
    /// its names, `["each", "to_lower"]`), `unique` (`true`, only for a unique index), then the
    /// unknown keys. A definition of an unknown
    /// type or transform read
    /// from a payload gives that payload back unchanged.
    pub fn to_payload(&self) -> PropertyMap {
        if let Some(original) = &self.original {
            return original.clone();
        }
        let mut map = PropertyMap::default();
        map.insert("element", self.element.as_str().into());
        if let Some(label) = &self.label {
            map.insert("label", label.as_str().into());
        }
        map.insert("key", self.key.as_str().into());
        map.insert("type", self.index_type.as_str().into());
        if let Some(transform) = &self.transform {
            map.insert("transform", transform_payload(transform));
        }
        if self.unique {
            map.insert("unique", ElementProperty::Boolean(true));
        }
        for (key, value) in &self.extra {
            map.insert_if_absent(key, || value.clone());
        }
        map
    }

    /// Reads a payload map (the inverse of [`Self::to_payload`]). A missing `element` is
    /// `"vertex"`, a missing `label` a label-less index, a missing `type` is
    /// `"memory-ordered"`, a missing `transform` is none, a missing `unique` is `false`; a known
    /// key of the wrong type is an
    /// error; unknown keys are kept. A type or transform this version does not know is
    /// [`PropertyIndexType::Unknown`] / [`PropertyIndexTransform::Unknown`] (never an error),
    /// and the whole map is kept to be written back unchanged. Not validated.
    pub fn from_payload(payload: &PropertyMap) -> Result<PropertyIndexDefinition, DefinitionError> {
        Self::read_fields_of(DefinitionKind::PROPERTY_INDEX_EXT, payload, &[])
    }

    /// Reads the fields of a definition of `kind`: a [`DefinitionKind::PROPERTY_INDEX`] needs
    /// its `label` (a payload without one is `Malformed`, kept opaque by a load as 0.5.x does);
    /// a [`DefinitionKind::PROPERTY_INDEX_EXT`] without one is label-less.
    pub(crate) fn read_fields_of(
        kind: DefinitionKind,
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
        let label = text("label")?;
        if label.is_none() && kind == DefinitionKind::PROPERTY_INDEX {
            return Err(DefinitionError::Malformed {
                reason: "'label' is missing".to_string(),
            });
        }
        let key = text("key")?.ok_or_else(|| DefinitionError::Malformed {
            reason: "'key' is missing".to_string(),
        })?;
        let index_type = match text("type")? {
            None => PropertyIndexType::MemoryOrdered,
            Some(name) => PropertyIndexType::from_name(&name),
        };
        // One name, or a chain as the array of its names.
        let transform = match map.get("transform") {
            None => None,
            Some(ElementProperty::Array(names)) => {
                let names = names
                    .iter()
                    .map(|name| name.text().map(|text| text.into_owned()))
                    .collect::<Option<Vec<String>>>()
                    .ok_or_else(|| {
                        malformed("transform", "every name of a chain must be a string")
                    })?;
                Some(PropertyIndexTransform::from_names(&names).ok_or_else(|| {
                    malformed("transform", "a chain names at least one transform")
                })?)
            }
            Some(_) => text("transform")?.map(|name| PropertyIndexTransform::from_name(&name)),
        };
        let unique = match map.get("unique") {
            None => false,
            Some(ElementProperty::Boolean(flag)) => *flag,
            Some(_) => return Err(malformed("unique", "must be a boolean")),
        };
        let mut index = match label {
            Some(label) => PropertyIndexDefinition::new(element, label, key),
            None => PropertyIndexDefinition::label_less(element, key),
        }
        .with_type(index_type)
        .with_transform(transform)
        .with_unique(unique);
        index.unknown_element = unknown_element;
        const KNOWN: [&str; 6] = ["element", "label", "key", "type", "transform", "unique"];
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

    /// The rules of [`Definition::validate`] for an index named `name` stored as `kind` with
    /// `flags`: a label-less index must be [`DefinitionKind::PROPERTY_INDEX_EXT`], which is
    /// always critical.
    pub(crate) fn validate(
        &self,
        kind: DefinitionKind,
        flags: DefinitionFlags,
        name: &str,
    ) -> Result<(), DefinitionError> {
        let field = |field: &str, reason: String| DefinitionError::InvalidField {
            name: name.to_string(),
            field: field.to_string(),
            reason,
        };
        if kind == DefinitionKind::PROPERTY_INDEX && self.needs_extended_kind() {
            return Err(if self.label.is_none() {
                field(
                    "label",
                    "a label-less index is stored as kind property_index_ext \
                     (Definition::property_index picks it)"
                        .to_string(),
                )
            } else if self.unique {
                field(
                    "unique",
                    "a unique index is stored as kind property_index_ext \
                     (Definition::property_index picks it)"
                        .to_string(),
                )
            } else {
                field(
                    "transform",
                    "an index with the transform each is stored as kind property_index_ext \
                     (Definition::property_index picks it)"
                        .to_string(),
                )
            });
        }
        if kind == DefinitionKind::PROPERTY_INDEX_EXT && !flags.is_critical() {
            return Err(field(
                "flags",
                "an index of kind property_index_ext is always critical".to_string(),
            ));
        }
        if let Some(label) = &self.label {
            if label.is_empty() {
                return Err(field(
                    "label",
                    "it is empty; omit label for an index of every element of the kind".to_string(),
                ));
            }
            if label.len() > MAX_INDEX_LABEL_BYTES {
                return Err(field(
                    "label",
                    format!("it is longer than {MAX_INDEX_LABEL_BYTES} bytes"),
                ));
            }
            if label.contains("::") {
                return Err(field(
                    "label",
                    "a label cannot contain '::' (the multi-label separator)".to_string(),
                ));
            }
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
    /// A property index named `name`: kind [`DefinitionKind::PROPERTY_INDEX`], not critical
    /// (the index holds no data of its own, a reader that does not know the kind loses nothing
    /// but the speed-up), or, for an index that
    /// [needs the extended kind](PropertyIndexDefinition::needs_extended_kind) (a label-less
    /// one), kind [`DefinitionKind::PROPERTY_INDEX_EXT`] with the critical flag (a reader that
    /// does not know it opens the store read-only).
    pub fn property_index(name: impl Into<String>, index: PropertyIndexDefinition) -> Self {
        let kind = index.definition_kind();
        Definition::property_index_of(kind, name.into(), index)
    }

    /// A property index stored as `kind` (a load keeps the stored kind): critical for
    /// [`DefinitionKind::PROPERTY_INDEX_EXT`].
    pub(crate) fn property_index_of(
        kind: DefinitionKind,
        name: String,
        index: PropertyIndexDefinition,
    ) -> Self {
        let flags = DefinitionFlags::NONE.with_critical(kind == DefinitionKind::PROPERTY_INDEX_EXT);
        Definition::from_parts(kind, flags, name, DefinitionBody::PropertyIndex(index))
    }

    /// The property index, when this is a decoded one.
    pub fn as_property_index(&self) -> Option<&PropertyIndexDefinition> {
        match self.body() {
            DefinitionBody::PropertyIndex(index) => Some(index),
            DefinitionBody::Query(_)
            | DefinitionBody::Compression(_)
            | DefinitionBody::Job(_)
            | DefinitionBody::Model(_)
            | DefinitionBody::Opaque(_) => None,
        }
    }
}

/// Checks a property index definition against the catalog it is about to join: no other index
/// of the same `(element, label, key, type, transform)` (indexes of different types or
/// transforms on one key, such as a plain one and a `to_lower` one, may coexist; so do a
/// label-less and a per-label index on one key). An index of the same name in either index
/// kind is the one it replaces.
pub(crate) fn check_conflict(
    graph: &TraversalGraph,
    definition: &Definition,
) -> Result<(), GraphError> {
    let Some(index) = definition.as_property_index() else {
        return Ok(());
    };
    for other in graph.definitions.iter() {
        if !other.kind().is_property_index() || other.name() == definition.name() {
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
                    "the index '{}' already covers {} on '{}' (type '{}'{})",
                    other.name(),
                    match index.label() {
                        Some(label) => format!("{} '{label}'", index.element_name()),
                        None => format!("every {}", index.element_name()),
                    },
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
    /// Index entries: one per element with a scalar value at the key; with the transform
    /// `each` one per distinct element of an array value (an element held twice counts once).
    pub entries: u64,
    /// Distinct indexed values.
    pub distinct_values: u64,
    /// An estimate of the index's heap footprint, in bytes.
    pub bytes: u64,
    /// Elements of the index's label whose value at the key is not `null` and not held by the
    /// index (an array, an object or a vector; with `to_lower` every non-string value; with
    /// `each` one per element of an array that is not held: a nested array, an object, a
    /// vector, with `each to_lower` a non-string; an object or a vector value itself). The
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
    /// The label it covers; `None` for a label-less index (every element of the kind,
    /// unlabeled ones included).
    pub label: Option<String>,
    /// The top-level property key it orders by.
    pub key: String,
    /// The type of the index (a storage offers only indexes of types it serves).
    pub index_type: PropertyIndexType,
    /// The transform of the index (`None`: the values themselves).
    pub transform: Option<PropertyIndexTransform>,
    /// Whether the index is a unique constraint
    /// ([`PropertyIndexDefinition::with_unique`]) the storage enforces: a storage lists a
    /// unique index only while it enforces it (an index whose data holds repeated values is
    /// not served, see [`GraphStorage::unique_index_duplicates`]).
    ///
    /// [`GraphStorage::unique_index_duplicates`]: crate::graph::GraphStorage::unique_index_duplicates
    pub unique: bool,
    /// What it holds right now.
    pub stats: PropertyIndexStats,
}

impl PropertyIndexInfo {
    /// An entry with these parts (for a storage outside this crate), of type
    /// [`PropertyIndexType::MemoryOrdered`] (see [`Self::with_type`]); `label` `None` for a
    /// label-less index.
    pub fn new(
        name: impl Into<String>,
        element: ElementKind,
        label: Option<&str>,
        key: impl Into<String>,
        stats: PropertyIndexStats,
    ) -> Self {
        PropertyIndexInfo {
            name: name.into(),
            element,
            label: label.map(str::to_string),
            key: key.into(),
            index_type: PropertyIndexType::MemoryOrdered,
            transform: None,
            unique: false,
            stats,
        }
    }

    /// The same entry with (`true`) or without the unique constraint.
    pub fn with_unique(mut self, unique: bool) -> Self {
        self.unique = unique;
        self
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

/// One value a unique property index finds held by several elements (a
/// [`UniqueIndexDuplicates`] report).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DuplicateValue {
    /// The value as JSON text (`"alice@example.com"`, `42`), transformed as the index keys it
    /// (the lowered text for `to_lower`).
    pub value: String,
    /// How many elements hold it.
    pub count: u64,
    /// The ids of up to [`UniqueIndexDuplicates::MAX_EXAMPLES`] of them.
    pub examples: Vec<String>,
}

impl DuplicateValue {
    /// The entry of `value` (rendered as JSON text) held by `count` elements, `examples` some
    /// of their ids (for a storage outside this crate).
    pub fn new(value: &ElementProperty, count: u64, examples: Vec<String>) -> Self {
        DuplicateValue {
            value: crate::value::physical::to_json_text(&value.to_logical())
                .unwrap_or_else(|err| err.to_string()),
            count,
            examples,
        }
    }
}

impl std::fmt::Display for DuplicateValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} elements", self.value, self.count)?;
        if !self.examples.is_empty() {
            write!(f, ": {}", self.examples.join(", "))?;
            if (self.examples.len() as u64) < self.count {
                f.write_str(", ...")?;
            }
        }
        f.write_str(")")
    }
}

/// Why a unique property index is not served: values its data holds more than once (book page
/// "Property Indexes", section "Unique indexes"). A create over such data is refused with
/// [`DefinitionError::UniqueIndexDuplicates`]; an index stored without that check (a load, a
/// replay, [`GraphStorage::set_definition`]) is kept but not served nor enforced
/// ([`GraphStorage::unique_index_duplicates`]).
///
/// [`GraphStorage::set_definition`]: crate::graph::GraphStorage::set_definition
/// [`GraphStorage::unique_index_duplicates`]: crate::graph::GraphStorage::unique_index_duplicates
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct UniqueIndexDuplicates {
    /// Up to [`Self::MAX_VALUES`] repeated values, in index order.
    pub values: Vec<DuplicateValue>,
    /// How many distinct values repeat in all.
    pub total: u64,
}

impl UniqueIndexDuplicates {
    /// The most values a report lists.
    pub const MAX_VALUES: usize = 10;
    /// The most example ids a value lists.
    pub const MAX_EXAMPLES: usize = 3;

    /// A report of `values` (the first ones) out of `total` repeated values (for a storage
    /// outside this crate).
    pub fn new(values: Vec<DuplicateValue>, total: u64) -> Self {
        UniqueIndexDuplicates { values, total }
    }

    /// The values as one line: `"a" (2 elements: 1, 4), 7 (3 elements: 2, 5, 9)`, with
    /// `and N more` when the report lists only the first ones.
    pub fn summary(&self) -> String {
        let mut text = self
            .values
            .iter()
            .map(DuplicateValue::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let more = self.total.saturating_sub(self.values.len() as u64);
        if more > 0 {
            text.push_str(&format!(" and {more} more"));
        }
        text
    }

    /// The `reason` front ends list next to `served: false` for such an index: it starts with
    /// `duplicates:`, names the values and the fix (remove the repeats, then drop the index and
    /// create it again).
    pub fn unserved_reason(&self) -> String {
        format!(
            "duplicates: the data holds {} repeated value(s): {}; the unique index is neither \
             served nor enforced, queries scan. Give the repeated elements other values, then \
             drop the index and create it again",
            self.total,
            self.summary()
        )
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
            | ElementProperty::Blob(_)
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

/// The key range of the postings map for set bounds: the slot part of `(key, slot)` bounds is
/// always 0 or `u64::MAX` (the whole postings of a bound key in or out), so dropping it keeps
/// the same keys. `None` for a range `BTreeMap::range` would refuse (it panics on a start past
/// the end, or on equal excluded bounds).
fn key_bounds((start, end): SetBounds) -> Option<(Bound<IndexKey>, Bound<IndexKey>)> {
    let key = |bound: Bound<(IndexKey, u64)>| match bound {
        Bound::Included((key, _)) => Bound::Included(key),
        Bound::Excluded((key, _)) => Bound::Excluded(key),
        Bound::Unbounded => Bound::Unbounded,
    };
    let (start, end) = (key(start), key(end));
    let refused = match (&start, &end) {
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b))
            if a > b =>
        {
            true
        }
        (Bound::Excluded(a), Bound::Excluded(b)) => a == b,
        _ => false,
    };
    (!refused).then_some((start, end))
}

// ---------------------------------------------------------------------------------------------
// The in-memory state
// ---------------------------------------------------------------------------------------------

/// One active index: each key once with the slots of its elements (by key, then slot).
#[derive(Debug, Clone)]
struct IndexState {
    name: CompactString,
    element: ElementKind,
    /// `None`: label-less (every element of the kind).
    label: Option<CompactString>,
    key: CompactString,
    transform: Option<PropertyIndexTransform>,
    /// A unique constraint, enforced while the index is installed (built without duplicates).
    unique: bool,
    entries: BTreeMap<IndexKey, Postings>,
    /// The runs of the postings with two or more slots.
    runs: RunsStore,
    /// The number of (key, slot) entries, the sum of the postings' lengths.
    len: u64,
    /// The heap bytes of the keys and the postings, kept with every write: the stats are read on
    /// every execution (by the optimizer), so they never walk the index.
    heap: u64,
    /// Elements whose non-null value at the key the index does not hold.
    skipped: u64,
}

impl IndexState {
    fn new(
        name: &str,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<PropertyIndexTransform>,
        unique: bool,
    ) -> IndexState {
        IndexState {
            name: CompactString::from(name),
            element,
            label: label.map(CompactString::from),
            key: CompactString::from(key),
            transform,
            unique,
            entries: BTreeMap::new(),
            runs: RunsStore::default(),
            len: 0,
            heap: 0,
            skipped: 0,
        }
    }

    /// Puts in (`insert`) or takes out the entry of a stored value of the element at `slot`: its
    /// key, or the `skipped` count for a non-null value the index does not hold.
    /// With `each` an array puts in (takes out) one key per element, an element held twice
    /// once (the postings hold a slot once per key).
    fn account(&mut self, insert: bool, value: &ElementProperty, slot: u32) {
        let transform = self.transform.take();
        let skipped = value_keys(transform.as_ref(), value, |key| {
            if insert {
                self.insert(key, slot);
            } else {
                self.remove(&key, slot);
            }
        });
        self.transform = transform;
        if insert {
            self.skipped += skipped;
        } else {
            self.skipped = self.skipped.saturating_sub(skipped);
        }
    }

    fn insert(&mut self, key: IndexKey, slot: u32) {
        match self.entries.entry(key) {
            std::collections::btree_map::Entry::Vacant(vacant) => {
                self.heap += vacant.key().heap_bytes();
                vacant.insert(Postings::One(slot));
                self.len += 1;
            }
            std::collections::btree_map::Entry::Occupied(mut occupied) => {
                let postings = occupied.get_mut();
                let before = self.runs.heap_bytes(postings);
                if self.runs.insert(postings, slot) {
                    let after = self.runs.heap_bytes(postings);
                    self.heap = (self.heap + after).saturating_sub(before);
                    self.len += 1;
                }
            }
        }
    }

    fn remove(&mut self, key: &IndexKey, slot: u32) {
        let Some(postings) = self.entries.get_mut(key) else {
            return;
        };
        let before = self.runs.heap_bytes(postings);
        match self.runs.remove(postings, slot) {
            Removed::Absent => return,
            Removed::Remaining => {
                let after = self.runs.heap_bytes(postings);
                self.heap = (self.heap + after).saturating_sub(before);
            }
            Removed::Emptied => {
                self.heap = self.heap.saturating_sub(before + key.heap_bytes());
                self.entries.remove(key);
            }
        }
        self.len = self.len.saturating_sub(1);
    }

    /// Gives back the spare capacity of every postings (after a build) and recounts the heap.
    fn shrink(&mut self) {
        self.runs.shrink();
        self.heap = self.walked_heap();
    }

    /// Whether both hold the same keys with the same slots (whatever their runs look like).
    fn same_entries(&self, other: &IndexState) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(&other.entries)
                .all(|((a, pa), (b, pb))| a == b && self.runs.iter(pa).eq(other.runs.iter(pb)))
    }

    /// The heap bytes of the keys and the postings, walked from the real containers.
    fn walked_heap(&self) -> u64 {
        self.entries
            .iter()
            .map(|(key, postings)| key.heap_bytes() + self.runs.heap_bytes(postings))
            .sum()
    }

    /// The number of elements inside `range` (exact: the sum of the postings' lengths of the
    /// keys in it); `None` for an `each` index whose slice holds more than one key (an element
    /// may be held under several of them).
    fn count(&self, range: &PropertyIndexRange) -> Option<u64> {
        let Some(bounds) = set_bounds(range).and_then(key_bounds) else {
            return Some(0);
        };
        let mut keys = self.entries.range(bounds);
        if self.has_each() {
            let first = keys.next();
            if keys.next().is_some() {
                return None;
            }
            return Some(first.map_or(0, |(_, postings)| self.runs.len(postings) as u64));
        }
        Some(
            keys.map(|(_, postings)| self.runs.len(postings) as u64)
                .sum(),
        )
    }

    /// The slot of an element other than `me` holding a key equal to `key` (`has()`'s
    /// equality: an `int64` and a `float64` of the same value are equal, two `int64`s only when
    /// they are the same number); `None` for a `null` key, which is not part of uniqueness.
    fn other_holder(&self, key: &IndexKey, me: Option<u32>) -> Option<u32> {
        let other = |postings: &Postings| self.runs.iter(postings).find(|slot| Some(*slot) != me);
        match key {
            IndexKey::Null => None,
            IndexKey::Number(value, kind, int) => {
                let lower = IndexKey::Number(*value, 0, i64::MIN);
                let upper = IndexKey::Number(*value, 1, i64::MAX);
                self.entries
                    .range(lower..=upper)
                    .filter(|(held, _)| match held {
                        IndexKey::Number(_, held_kind, held_int) => {
                            held_kind != kind || *kind == 1 || held_int == int
                        }
                        _ => false,
                    })
                    .find_map(|(_, postings)| other(postings))
            }
            _ => self.entries.get(key).and_then(other),
        }
    }

    /// The groups of slots holding an equal non-null key (by [`Self::other_holder`]'s
    /// equality), each with two or more slots, in key order: what keeps a unique index from
    /// being built.
    fn duplicate_groups(&self) -> Vec<Vec<u32>> {
        let mut groups = Vec::new();
        let mut keys = self.entries.iter().peekable();
        while let Some((key, postings)) = keys.next() {
            match key {
                IndexKey::Null => {}
                IndexKey::Number(value, _, _) => {
                    // Every number key of the same float64 value: a float equals each of them,
                    // two ints only themselves.
                    let mut members = vec![(key, postings)];
                    while let Some((IndexKey::Number(next, _, _), _)) = keys.peek()
                        && next == value
                    {
                        if let Some(member) = keys.next() {
                            members.push(member);
                        }
                    }
                    if members
                        .iter()
                        .any(|(key, _)| matches!(key, IndexKey::Number(_, 1, _)))
                    {
                        let mut slots: Vec<u32> = members
                            .iter()
                            .flat_map(|(_, postings)| self.runs.iter(postings))
                            .collect();
                        slots.sort_unstable();
                        slots.dedup();
                        if slots.len() >= 2 {
                            groups.push(slots);
                        }
                    } else {
                        for (_, postings) in members {
                            if self.runs.len(postings) >= 2 {
                                groups.push(self.runs.iter(postings).collect());
                            }
                        }
                    }
                }
                _ => {
                    if self.runs.len(postings) >= 2 {
                        groups.push(self.runs.iter(postings).collect());
                    }
                }
            }
        }
        groups
    }

    /// Whether the index keys array elements (its transform starts with `each`).
    fn has_each(&self) -> bool {
        self.transform
            .as_ref()
            .is_some_and(PropertyIndexTransform::has_each)
    }

    /// The stats: per key its map entry (the key, the 8-byte postings and a B-tree share of 8
    /// bytes), plus the heap of the keys and the runs (the real containers' capacities, kept
    /// with every write and compared with a walk by `check_property_indexes`) and the table of
    /// runs.
    fn stats(&self) -> PropertyIndexStats {
        let entry = (std::mem::size_of::<IndexKey>() + std::mem::size_of::<Postings>()) as u64 + 8;
        PropertyIndexStats {
            entries: self.len,
            distinct_values: self.entries.len() as u64,
            bytes: self.entries.len() as u64 * entry + self.heap + self.runs.table_bytes(),
            skipped: self.skipped,
        }
    }
}

/// The indexes of a graph (derived from the catalog), with lookups by element kind and label
/// (`vertex_any` / `edge_any`: the label-less ones).
#[derive(Debug, Clone, Default)]
pub(crate) struct PropertyIndexes {
    indexes: Vec<IndexState>,
    vertex: HashMap<CompactString, Vec<usize>>,
    edge: HashMap<CompactString, Vec<usize>>,
    vertex_any: Vec<usize>,
    edge_any: Vec<usize>,
    /// How many installed indexes are unique (enforced): `0` makes every write's unique check
    /// one branch.
    unique: usize,
    /// The unique indexes of the catalog that are not installed because their data holds
    /// repeated values (not served, not enforced), with the report, by name.
    disabled: Vec<(CompactString, UniqueIndexDuplicates)>,
}

/// An element other than the one written already holds the key a unique index would get:
/// the index (its position), the other element's slot and the value written.
pub(crate) struct UniqueConflict {
    position: usize,
    slot: u32,
    value: ElementProperty,
}

impl PropertyIndexes {
    /// Whether no index exists at all.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }

    /// Whether an enforced unique index exists.
    #[inline]
    pub(crate) fn has_unique(&self) -> bool {
        self.unique > 0
    }

    /// The first unique index of an element of kind `element` (its slot `me`, `None` for a new
    /// element) labelled `labels` whose key another element already holds: with `write`
    /// `Some((key, value))` the indexes on `key` with `value` as the element's value there
    /// (the other keys are unchanged), else every unique index of the labels with the values of
    /// `properties`. A key the index does not hold (`null`, a skipped value) never conflicts.
    #[inline(never)]
    pub(crate) fn unique_conflict<'l>(
        &self,
        element: ElementKind,
        me: Option<u32>,
        labels: impl IntoIterator<Item = &'l str>,
        properties: Option<&PropertyMap>,
        write: Option<(&str, &ElementProperty)>,
    ) -> Option<UniqueConflict> {
        let (by_label, any) = match element {
            ElementKind::Vertex => (&self.vertex, &self.vertex_any),
            ElementKind::Edge => (&self.edge, &self.edge_any),
        };
        let check = |position: usize| -> Option<UniqueConflict> {
            let index = self.indexes.get(position).filter(|index| index.unique)?;
            let value = match write {
                Some((key, value)) if key == index.key.as_str() => value,
                Some(_) => return None,
                None => properties?.get(index.key.as_str())?,
            };
            // A unique index never has `each` (refused at create, never served): one key.
            let key = transformed_key(index.transform.as_ref(), value)?;
            let slot = index.other_holder(&key, me)?;
            Some(UniqueConflict {
                position,
                slot,
                value: value.clone(),
            })
        };
        if let Some(conflict) = any.iter().find_map(|position| check(*position)) {
            return Some(conflict);
        }
        labels
            .into_iter()
            .filter_map(|label| by_label.get(label))
            .flatten()
            .find_map(|position| check(*position))
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
        self.vertex_any.clear();
        self.edge_any.clear();
        for (position, index) in self.indexes.iter().enumerate() {
            let (map, any) = match index.element {
                ElementKind::Vertex => (&mut self.vertex, &mut self.vertex_any),
                ElementKind::Edge => (&mut self.edge, &mut self.edge_any),
            };
            match &index.label {
                Some(label) => map.entry(label.clone()).or_default().push(position),
                None => any.push(position),
            }
        }
        self.unique = self.indexes.iter().filter(|index| index.unique).count();
    }

    /// Takes the entries of one element out (`insert` false) or puts them in (`insert` true):
    /// the element's labels and properties as stored; `only_key` limits it to one top-level key.
    fn element_entries<'l>(
        &mut self,
        element: ElementKind,
        insert: bool,
        slot: u32,
        labels: impl Iterator<Item = &'l str>,
        properties: Option<&PropertyMap>,
        only_key: Option<&str>,
    ) {
        let Some(properties) = properties else {
            return;
        };
        let (by_label, any) = match element {
            ElementKind::Vertex => (&self.vertex, &self.vertex_any),
            ElementKind::Edge => (&self.edge, &self.edge_any),
        };
        // The label-less indexes hold the element once, whatever its labels.
        for position in any {
            let Some(index) = self.indexes.get_mut(*position) else {
                continue;
            };
            if only_key.is_some_and(|key| key != index.key.as_str()) {
                continue;
            }
            let Some(value) = properties.get(index.key.as_str()) else {
                continue;
            };
            index.account(insert, value, slot);
        }
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
                index.account(insert, value, slot);
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
            node.index().slot(),
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
            edge.index().slot(),
            label.into_iter(),
            properties,
            only_key,
        );
    }

    /// The index on `(element, label, key, transform)` (`label` `None`: the label-less one).
    fn find(
        &self,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
    ) -> Option<&IndexState> {
        let positions = match (label, element) {
            (Some(label), _) => self.by_label(element).get(label)?,
            (None, ElementKind::Vertex) => &self.vertex_any,
            (None, ElementKind::Edge) => &self.edge_any,
        };
        positions
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
        let mut state = IndexState::new(
            name,
            definition.element(),
            definition.label(),
            definition.key(),
            definition.transform().cloned(),
            definition.is_unique(),
        );
        let Some(label) = definition.label() else {
            // Label-less: every element of the kind, unlabeled ones included.
            match definition.element() {
                ElementKind::Vertex => {
                    for (index, vertex) in self.graph.node_references() {
                        if let Some(value) = vertex.2.get(definition.key()) {
                            state.account(true, value, index.slot());
                        }
                    }
                }
                ElementKind::Edge => {
                    for edge in self.graph.edge_references() {
                        if let Some(value) = edge
                            .weight()
                            .2
                            .as_ref()
                            .and_then(|properties| properties.get(definition.key()))
                        {
                            state.account(true, value, edge.id().slot());
                        }
                    }
                }
            }
            state.shrink();
            return state;
        };
        match definition.element() {
            ElementKind::Vertex => {
                if let Some(nodes) = self.index.get_nodes_by_label(label) {
                    for node in nodes {
                        if let Some(vertex) = self.vertex_weight(node)
                            && let Some(value) = vertex.2.get(definition.key())
                        {
                            state.account(true, value, node.index().slot());
                        }
                    }
                }
            }
            ElementKind::Edge => {
                if let Some(edges) = self.index.get_edges_by_label(label) {
                    for edge in edges {
                        if let Some(weight) = self.edge_weight(edge)
                            && let Some(value) = weight
                                .2
                                .as_ref()
                                .and_then(|properties| properties.get(definition.key()))
                        {
                            state.account(true, value, edge.index().slot());
                        }
                    }
                }
            }
        }
        state.shrink();
        state
    }

    /// Builds one index from the data; a unique one whose data holds repeated values gives the
    /// report instead (it is then neither served nor enforced).
    fn build_checked(
        &self,
        name: &str,
        definition: &PropertyIndexDefinition,
    ) -> Result<IndexState, UniqueIndexDuplicates> {
        let state = self.build_index(name, definition);
        if !state.unique {
            return Ok(state);
        }
        let groups = state.duplicate_groups();
        if groups.is_empty() {
            return Ok(state);
        }
        tracing::warn!(
            index = name,
            repeated = groups.len(),
            "a unique property index is not served: its data holds repeated values"
        );
        Err(self.duplicates_report(&state, &groups))
    }

    /// The report of the duplicate `groups` of `state`: the first values with their counts
    /// and example ids.
    fn duplicates_report(&self, state: &IndexState, groups: &[Vec<u32>]) -> UniqueIndexDuplicates {
        let values = groups
            .iter()
            .take(UniqueIndexDuplicates::MAX_VALUES)
            .map(|slots| {
                let examples: Vec<String> = slots
                    .iter()
                    .take(UniqueIndexDuplicates::MAX_EXAMPLES)
                    .filter_map(|slot| self.slot_id(state.element, *slot))
                    .collect();
                let value = slots
                    .first()
                    .and_then(|slot| self.slot_value(state.element, *slot, &state.key))
                    .map(|value| keyed_value(state.transform.as_ref(), &value))
                    .unwrap_or(ElementProperty::Null);
                DuplicateValue::new(&value, slots.len() as u64, examples)
            })
            .collect();
        UniqueIndexDuplicates::new(values, groups.len() as u64)
    }

    /// The id of the live element of kind `element` at `slot`.
    fn slot_id(&self, element: ElementKind, slot: u32) -> Option<String> {
        match element {
            ElementKind::Vertex => self
                .graph
                .node_weight(NodeIndex::from_slot(slot))
                .map(|vertex| vertex.0.to_string()),
            ElementKind::Edge => self
                .graph
                .edge_weight(EdgeIndex::from_slot(slot))
                .map(|edge| edge.0.to_string()),
        }
    }

    /// The logical value at top-level `key` of the live element of kind `element` at `slot`.
    fn slot_value(&self, element: ElementKind, slot: u32, key: &str) -> Option<ElementProperty> {
        match element {
            ElementKind::Vertex => self
                .graph
                .node_weight(NodeIndex::from_slot(slot))
                .and_then(|vertex| vertex.2.get(key))
                .map(ElementProperty::to_logical),
            ElementKind::Edge => self
                .graph
                .edge_weight(EdgeIndex::from_slot(slot))
                .and_then(|edge| edge.2.as_ref()?.get(key))
                .map(ElementProperty::to_logical),
        }
    }

    /// The error of a unique `conflict` ([`PropertyIndexes::unique_conflict`]): names the
    /// index, the key, the value written and the element holding it.
    #[cold]
    pub(crate) fn unique_violation(&self, conflict: UniqueConflict) -> GraphError {
        let Some(index) = self.property_indexes.indexes.get(conflict.position) else {
            return GraphError::invalid_element_state("a unique index moved during a write");
        };
        GraphError::unique_violation(
            index.name.as_str(),
            index.element,
            index.key.as_str(),
            &conflict.value,
            self.slot_id(index.element, conflict.slot)
                .unwrap_or_default(),
        )
    }

    /// The report of the unique index `name` when it is not served because its data holds
    /// repeated values ([`GraphStorage::unique_index_duplicates`]).
    ///
    /// [`GraphStorage::unique_index_duplicates`]: crate::graph::GraphStorage::unique_index_duplicates
    pub(crate) fn property_index_duplicates(&self, name: &str) -> Option<UniqueIndexDuplicates> {
        self.property_indexes
            .disabled
            .iter()
            .find(|(disabled, _)| disabled == name)
            .map(|(_, report)| report.clone())
    }

    /// After a catalog change of kind property index: rebuilds the index `name` from the data
    /// (or removes it when the catalog no longer holds it).
    pub(crate) fn property_index_after_change(&mut self, name: &str) {
        // An index of a type this version does not know is kept in the catalog, never built.
        // Index names are unique across both index kinds.
        let definition = self
            .definitions
            .get(DefinitionKind::PROPERTY_INDEX, name)
            .or_else(|| {
                self.definitions
                    .get(DefinitionKind::PROPERTY_INDEX_EXT, name)
            })
            .and_then(|definition| definition.as_property_index().cloned())
            .filter(PropertyIndexDefinition::is_served_type);
        // A unique index over repeated values is kept disabled (not installed) with its report.
        self.property_indexes
            .disabled
            .retain(|(disabled, _)| disabled != name);
        let built = match definition.map(|definition| self.build_checked(name, &definition)) {
            Some(Ok(state)) => Some(state),
            Some(Err(report)) => {
                self.property_indexes
                    .disabled
                    .push((CompactString::from(name), report));
                None
            }
            None => None,
        };
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
        let mut indexes = Vec::with_capacity(definitions.len());
        let mut disabled = Vec::new();
        for (name, definition) in &definitions {
            match self.build_checked(name, definition) {
                Ok(state) => indexes.push(state),
                Err(report) => disabled.push((CompactString::from(name.as_str()), report)),
            }
        }
        self.property_indexes = PropertyIndexes {
            indexes,
            disabled,
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
                label: index.label.as_ref().map(CompactString::to_string),
                key: index.key.to_string(),
                index_type: PropertyIndexType::MemoryOrdered,
                transform: index.transform.clone(),
                unique: index.unique,
                stats: index.stats(),
            })
            .collect()
    }

    /// The slots in the index on `(element, label, key, transform)` (`label` `None`: the
    /// label-less index) inside `range`, by key then slot; `None` when there is no such index.
    fn index_slice<'a>(
        &'a self,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = u32> + 'a>> {
        let index = self.property_indexes.find(element, label, key, transform)?;
        let Some(bounds) = set_bounds(range).and_then(key_bounds) else {
            return Some(Box::new(std::iter::empty()));
        };
        if index.has_each() {
            // D15: an element holding several values of the slice is in several postings; a
            // slice of more than one key is collected and deduplicated (by slot).
            let mut keys = index.entries.range(bounds.clone());
            let first = keys.next();
            if keys.next().is_some() {
                let mut slots: Vec<u32> = index
                    .entries
                    .range(bounds)
                    .flat_map(|(_, postings)| index.runs.iter(postings))
                    .collect();
                slots.sort_unstable();
                slots.dedup();
                return Some(Box::new(slots.into_iter()));
            }
            return Some(match first {
                Some((_, postings)) => Box::new(index.runs.iter(postings)),
                None => Box::new(std::iter::empty()),
            });
        }
        Some(Box::new(
            index
                .entries
                .range(bounds)
                .flat_map(|(_, postings)| index.runs.iter(postings)),
        ))
    }

    /// The vertices labelled `label` (every vertex: `None`) whose `key` (transformed by
    /// `transform`) lies in `range` (a superset), from the index on `(vertex, label, key,
    /// transform)`; `None` without such an index.
    pub(crate) fn property_index_vertices<'a>(
        &'a self,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = VertexHandle> + 'a>> {
        let slice = self.index_slice(ElementKind::Vertex, label, key, transform, range)?;
        // The generation is the slot's: exact maintenance names only live elements' slots.
        Some(Box::new(slice.filter_map(|slot| {
            let index = NodeIndex::from_slot(slot);
            let vertex = self.graph.node_weight(index)?;
            Some(VertexHandle::new(index, vertex.3))
        })))
    }

    /// The edge counterpart of [`Self::property_index_vertices`].
    pub(crate) fn property_index_edges<'a>(
        &'a self,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = EdgeHandle> + 'a>> {
        let slice = self.index_slice(ElementKind::Edge, label, key, transform, range)?;
        Some(Box::new(slice.filter_map(|slot| {
            let index = EdgeIndex::from_slot(slot);
            let edge = self.graph.edge_weight(index)?;
            Some(EdgeHandle::new(index, edge.3.own))
        })))
    }

    /// The exact number of elements of the index on `(element, label, key, transform)` inside
    /// `range` (from the postings' lengths, no element is visited); `None` without such an
    /// index, and for an `each` index whose slice holds more than one key.
    pub(crate) fn property_index_count(
        &self,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<u64> {
        self.property_indexes
            .find(element, label, key, transform)?
            .count(range)
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
            // A unique index disabled by repeated values is not maintained.
            .filter(|(name, _)| {
                !self
                    .property_indexes
                    .disabled
                    .iter()
                    .any(|(disabled, _)| disabled == name)
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
                || state.unique != fresh.unique
            {
                return Err(format!("index '{name}': its definition differs"));
            }
            if !state.same_entries(&fresh) {
                return Err(format!(
                    "index '{name}': {} entries over {} keys maintained, {} over {} after a \
                     rebuild (or other values)",
                    state.len,
                    state.entries.len(),
                    fresh.len,
                    fresh.entries.len()
                ));
            }
            if state.heap != state.walked_heap() {
                return Err(format!(
                    "index '{name}': {} heap bytes counted, {} held",
                    state.heap,
                    state.walked_heap()
                ));
            }
            let counted: u64 = state
                .entries
                .values()
                .map(|postings| state.runs.len(postings) as u64)
                .sum();
            if state.len != counted || state.len != fresh.len {
                return Err(format!(
                    "index '{name}': {} entries counted, {counted} held, {} after a rebuild",
                    state.len, fresh.len
                ));
            }
            if state.skipped != fresh.skipped {
                return Err(format!(
                    "index '{name}': {} skipped values counted, {} after a rebuild",
                    state.skipped, fresh.skipped
                ));
            }
            if state.unique && !state.duplicate_groups().is_empty() {
                return Err(format!(
                    "index '{name}': unique, but its data holds repeated values"
                ));
            }
        }
        // Every disabled index is a unique index of the catalog.
        for (name, _) in &self.property_indexes.disabled {
            let unique = self.definitions.iter().any(|definition| {
                definition.name() == name.as_str()
                    && definition
                        .as_property_index()
                        .is_some_and(PropertyIndexDefinition::is_unique)
            });
            if !unique {
                return Err(format!(
                    "index '{name}' is disabled, but the catalog holds no unique index of that name"
                ));
            }
        }
        // The lookup tables match the indexes.
        let mut lookup = self.property_indexes.clone();
        lookup.reindex_lookup();
        if lookup.vertex != self.property_indexes.vertex
            || lookup.edge != self.property_indexes.edge
            || lookup.vertex_any != self.property_indexes.vertex_any
            || lookup.edge_any != self.property_indexes.edge_any
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
            .property_index_vertices(
                Some("person"),
                "age",
                None,
                &PropertyIndexRange::equal(int(29)),
            )
            .unwrap()
            .collect();
        assert_eq!(found, vec![v]);
        let mark = graph.begin_unit().unwrap();
        graph.set_vertex_property(v, "age".into(), int(30)).unwrap();
        graph.check_property_indexes().unwrap();
        graph
            .rollback_unit(mark, &crate::graph::RollbackReason::failed(&"test"))
            .unwrap();
        graph.check_property_indexes().unwrap();
        graph.drop_vertex(v).unwrap();
        graph.check_property_indexes().unwrap();
    }
}
