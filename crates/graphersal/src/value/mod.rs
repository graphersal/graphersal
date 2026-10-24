use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use crate::error::TraverserResult;
use crate::graph::GraphStorage;
use crate::graph::schema::GraphSchema;
use crate::{By, Traverser, TraverserContext};

/// [`Barrier`], the `Barrier.normSack` consumer token of `barrier(Consumer)`.
pub mod barrier;
/// [`Cardinality`], the `Cardinality.single/list/set` property-cardinality token.
pub mod cardinality;
/// The `as_string`/`as_bool`/`as_number` coercions ([`CastPolicy`]).
pub mod cast;
/// [`Column`], the `select(Column::Keys/Values)` token.
pub mod column;
/// [`CompressedText`], a string kept compressed in memory by a compression rule.
pub mod compressed;
/// [`DedupedText`], a string shared through a `dedup` compression rule's table.
pub mod deduped;
/// The value-depth limit: bounded nesting so recursive readers stay bounded.
pub(crate) mod depth;
/// [`Direction`], the `Direction.OUT/IN/BOTH` edge-direction token.
pub mod direction;
/// [`DT`], the unit/part token of the date and time steps.
pub mod dt;
/// [`VertexValue`]/[`EdgeValue`], a vertex or an edge returned as a value.
pub mod element_value;
/// Glob pattern compiler and segment matcher for the `glob_path()` step.
pub(crate) mod glob_path;
/// [`GType`], type tokens used by casts and predicates.
pub mod gtype;
/// [`JsonPath`], for the `json_path()` step.
pub mod json_path;
pub(crate) mod literal;
/// The `math()` expression engine (exp4j-compatible, plus nested access).
pub(crate) mod math_expr;
/// [`Merge`], the `Merge.onCreate/onMatch/outV/inV` option token of the upsert steps.
pub mod merge;
/// [`Number`], a numeric property value.
pub mod number;
/// [`Operator`], the TinkerPop `Operator.sum`/`Operator.addAll`/... binary-function token.
pub mod operator;
/// [`TraverserPath`], the path a traverser took through the graph.
pub mod path;
pub(crate) mod path_arena;
pub(crate) mod physical;
/// [`Pop`], the `Pop.first/last/all/mixed` label-occurrence token of `select`.
pub mod pop;
/// [`P`]/[`PValue`], the `P.*` predicates.
pub mod predicate;
mod prop_key;
/// [`PropertyMap`], a graph element's property map.
pub mod property_map;
/// [`RecordKey`], a key of a record map (`elementMap()`, `project()`, ...).
pub mod record_key;
pub(crate) mod sack;
pub(crate) mod scalar;
/// [`Scope`], the `Scope.local`/`Scope.global` token of scoped steps.
pub mod scope;
/// [`StreamMap`], the map a traversal produces (`group()`, `tree()`, ...).
pub mod stream_map;
/// [`Temporal`], the date and time values.
pub mod temporal;
pub(crate) mod uuid;
/// [`ValueMap`], the map value whose keys are values.
pub mod value_map;
/// [`Vector`], the vector (embedding) values.
pub mod vector;
/// [`WithOptions`], the `WithOptions.tokens/indexer/...` token of the step-level `with()`.
pub mod with_options;

pub use barrier::Barrier;
pub use cardinality::Cardinality;
pub use cast::CastPolicy;
pub use column::Column;
pub use compact_str::CompactString;
pub use compressed::CompressedText;
pub use deduped::DedupedText;
pub use direction::Direction;
pub use dt::DT;
pub use element_value::{EdgeValue, VertexRef, VertexValue};
pub use gtype::{GType, StreamKind};
pub use json_path::{JsonPath, JsonPathSegment};
pub use merge::Merge;
pub(crate) use number::N;
pub use number::Number;
pub use operator::Operator;
pub(crate) use path::TraverserPath;
pub(crate) use path_arena::{PathArena, PathRef};
pub use pop::Pop;
#[cfg(feature = "script")]
pub(crate) use predicate::TextRegex;
pub use predicate::{P, PValue};
pub use prop_key::{HasKey, PropKey, PropKeys};
pub use property_map::PropertyMap;
pub use record_key::RecordKey;
pub(crate) use sack::{SackArena, SackRef, SackSpec};
pub use scope::Scope;
pub use stream_map::StreamMap;
pub use temporal::Temporal;
pub use value_map::ValueMap;
pub use vector::embed::{
    EmbedContext, EmbedError, EmbedJob, EmbedJobError, EmbedReport, Embedder, EmbedderInfo,
    Embedders, EmbeddersBuilder, HashNgram,
};
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
pub use vector::embed::{Model2Vec, Model2VecOptions};
pub use vector::{Vector, VectorElement, VectorError, VectorMetric};
pub use with_options::WithOptions;

/// Represents strongly-typed property values attached to graph elements or computed stream items.
///
/// `#[non_exhaustive]`: a later variant (e.g. a temporal value) becomes a semver-minor addition instead
/// of a compile break for every consumer's `match`. Construction from other crates is unaffected,
/// since all variants remain public; only exhaustive matching outside this crate is prevented.
#[non_exhaustive]
pub enum ElementProperty {
    /// No value.
    Null,
    /// A UTF-8 string.
    String(CompactString),
    /// A numeric value (`i64` or `f64`).
    Number(Number),
    /// A boolean.
    Boolean(bool),
    /// Canonical RFC 9562 UUID, stored inline as a single 128-bit word (design decision D7).
    ///
    /// The 36-character canonical string exceeds `CompactString`'s 24-byte inline limit and would
    /// heap-allocate on every value; `u128` is zero-allocation and compares in one instruction.
    Uuid(u128),
    /// A date, time, date-time or duration ([`Temporal`], book "Dates and Times"), stored inline
    /// (plain integers, no allocation).
    Temporal(Temporal),
    /// A vector (an embedding: [`Vector`], book page "Vectors"): f32, f16, i8 or bit components
    /// behind an `Arc` (a clone shares them).
    Vector(Vector),
    /// A nested property map.
    Object(Box<PropertyMap>),
    /// A list of values.
    Array(Vec<ElementProperty>),
    /// A vertex returned as a value: id, labels and properties kept apart (never storable as a
    /// property).
    Vertex(Box<VertexValue>),
    /// An edge returned as a value: id, label, endpoints and properties kept apart (never
    /// storable as a property).
    Edge(Box<EdgeValue>),
    /// A map with at least one non-string key (`{29: 1}`, `{["josh"]: 1}`), insertion-ordered.
    ///
    /// Invariant: a `Map` always has a non-string key; a map whose keys are all strings is an
    /// [`ElementProperty::Object`]. The only constructors that uphold it are [`ElementProperty::from_entries`]
    /// and [`ValueMap::into_property`]. A `Map` is never storable as a property (`GraphError::InvalidPropertyValue`),
    /// unlike a string-keyed `Object` (a deliberate deviation, see the book page "Maps With Non-String Keys").
    ///
    /// `group()`, `groupCount()`, `tree()` and `unfold()` produce it when a key is not a string. A vertex or
    /// edge key is a snapshot (an element map) materialized once per result, not a live element as in
    /// TinkerPop (a deliberate deviation, same book page and `tinkerpop_deviations.md`).
    Map(Box<ValueMap>),
    /// A string the graph keeps compressed because a compression rule covers its path
    /// (`book/src/users_guide/compressed_properties.md`; book page "Compressed Properties").
    ///
    /// **Storage-only**, the way `Vertex`/`Edge` are stream-only: only the graph makes one, in a
    /// stored property map. It never appears in a traverser value, a script value, a
    /// `ChangeSet`, an export or a file: every read that leaves the graph goes through
    /// [`Self::logical`] (or [`Self::with_text`]), which yields the plain string. It is the
    /// string it holds for equality, hashing and ordering (`Compressed("x") == String("x")`), so
    /// no comparison can tell the two apart. A write of a `Compressed` value (a Rust caller that
    /// copied one out of
    /// [`GraphStorage::vertex_properties`]) is
    /// normalized under the graph's rules like any other string.
    Compressed(Box<CompressedText>),
    /// A string a `dedup` compression rule stores once per distinct value: the element keeps a
    /// reference to the text in the rule's table (`book/src/users_guide/compressed_properties.md`,
    /// section "Deduplicating repeated values").
    ///
    /// **Storage-only** like [`Self::Compressed`], with the same contract: every read that
    /// leaves the graph goes through [`Self::logical`] (or [`Self::with_text`]), which yields
    /// the plain string; it is the string it holds for equality, hashing and ordering. A write
    /// of a `Deduped` value is normalized under the graph's rules like any other string.
    Deduped(DedupedText),
}

impl Clone for ElementProperty {
    fn clone(&self) -> Self {
        match self {
            ElementProperty::Null => ElementProperty::Null,
            ElementProperty::String(text) => ElementProperty::String(text.clone()),
            ElementProperty::Number(number) => ElementProperty::Number(number.clone()),
            ElementProperty::Boolean(flag) => ElementProperty::Boolean(*flag),
            ElementProperty::Uuid(bits) => ElementProperty::Uuid(*bits),
            ElementProperty::Temporal(value) => ElementProperty::Temporal(*value),
            ElementProperty::Vector(value) => ElementProperty::Vector(value.clone()),
            ElementProperty::Object(map) => ElementProperty::Object(map.clone()),
            ElementProperty::Array(items) => ElementProperty::Array(items.clone()),
            ElementProperty::Vertex(map) => ElementProperty::Vertex(map.clone()),
            ElementProperty::Edge(map) => ElementProperty::Edge(map.clone()),
            ElementProperty::Map(map) => ElementProperty::Map(map.clone()),
            ElementProperty::Compressed(text) => clone_compressed(text),
            ElementProperty::Deduped(text) => ElementProperty::Deduped(text.clone()),
        }
    }
}

/// Clones a compressed value; out of line, so the recursive `clone` of nested values keeps the
/// stack frame it had before compressed values existed.
#[inline(never)]
#[cold]
fn clone_compressed(text: &CompressedText) -> ElementProperty {
    ElementProperty::Compressed(Box::new(text.clone()))
}

impl PartialEq for ElementProperty {
    /// Logical equality: a [`ElementProperty::Compressed`] value equals the string it holds.
    fn eq(&self, other: &Self) -> bool {
        use ElementProperty as E;
        match (self, other) {
            (E::Null, E::Null) => true,
            (E::String(a), E::String(b)) => a == b,
            (E::String(_), E::Compressed(_) | E::Deduped(_))
            | (E::Compressed(_) | E::Deduped(_), E::String(_))
            | (E::Compressed(_) | E::Deduped(_), E::Compressed(_) | E::Deduped(_)) => {
                compressed_eq(self, other)
            }
            (E::Number(a), E::Number(b)) => a == b,
            (E::Boolean(a), E::Boolean(b)) => a == b,
            (E::Uuid(a), E::Uuid(b)) => a == b,
            (E::Temporal(a), E::Temporal(b)) => a == b,
            (E::Vector(a), E::Vector(b)) => a == b,
            (E::Object(a), E::Object(b)) => a == b,
            (E::Array(a), E::Array(b)) => a == b,
            (E::Vertex(a), E::Vertex(b)) => a == b,
            (E::Edge(a), E::Edge(b)) => a == b,
            (E::Map(a), E::Map(b)) => a == b,
            (
                E::Null
                | E::String(_)
                | E::Number(_)
                | E::Boolean(_)
                | E::Uuid(_)
                | E::Temporal(_)
                | E::Vector(_)
                | E::Object(_)
                | E::Array(_)
                | E::Vertex(_)
                | E::Edge(_)
                | E::Map(_)
                | E::Compressed(_)
                | E::Deduped(_),
                _,
            ) => false,
        }
    }
}

impl Eq for ElementProperty {}

/// Hashes a compressed string as the string it holds; out of line (see [`compressed_eq`]).
#[inline(never)]
#[cold]
fn hash_compressed<H: std::hash::Hasher>(text: &CompressedText, state: &mut H) {
    text.with_text(|text| ElementProperty::hash_string_key(text, state))
}

/// Orders two string values (plain or compressed) by their text; out of line (see
/// [`compressed_eq`]).
#[inline(never)]
#[cold]
fn compare_text(a: &ElementProperty, b: &ElementProperty) -> std::cmp::Ordering {
    a.with_text(|a| b.with_text(|b| a.cmp(b)))
        .flatten()
        .unwrap_or(std::cmp::Ordering::Equal)
}

/// The equality of two string values of which at least one is compressed or deduplicated; out
/// of line so the recursive `eq` keeps a small stack frame. Two references to one shared text
/// are equal without reading it.
#[inline(never)]
#[cold]
fn compressed_eq(a: &ElementProperty, b: &ElementProperty) -> bool {
    match (a, b) {
        (ElementProperty::Compressed(a), ElementProperty::Compressed(b)) => a.logical_eq(b),
        (ElementProperty::Deduped(a), ElementProperty::Deduped(b)) => a.logical_eq(b),
        (ElementProperty::String(a), ElementProperty::Compressed(b))
        | (ElementProperty::Compressed(b), ElementProperty::String(a)) => b.eq_str(a),
        (ElementProperty::String(a), ElementProperty::Deduped(b))
        | (ElementProperty::Deduped(b), ElementProperty::String(a)) => a.as_str() == b.as_str(),
        (ElementProperty::Compressed(a), ElementProperty::Deduped(b))
        | (ElementProperty::Deduped(b), ElementProperty::Compressed(a)) => a.eq_str(b.as_str()),
        _ => false,
    }
}

impl ElementProperty {
    /// The logical value: `self` borrowed when it holds no [`ElementProperty::Compressed`]
    /// value at any depth (every scalar, every container without one), otherwise an owned copy
    /// with every compressed string decompressed. The boundary every stored value crosses when
    /// it leaves the graph (`book/src/users_guide/compressed_properties.md`).
    pub fn logical(&self) -> Cow<'_, ElementProperty> {
        if self.holds_compressed() {
            Cow::Owned(self.clone().into_logical())
        } else {
            Cow::Borrowed(self)
        }
    }

    /// An owned logical copy: [`Self::logical`] cloned (the stored-value counterpart of
    /// `clone()` at every read boundary).
    // Never inlined: it sits in recursive materializers (`into_node_property`).
    #[inline(never)]
    pub fn to_logical(&self) -> ElementProperty {
        if self.holds_compressed() {
            self.clone().into_logical()
        } else {
            self.clone()
        }
    }

    /// [`Self::logical`] of an owned value: every compressed string decompressed in place.
    #[inline(never)]
    pub fn into_logical(self) -> ElementProperty {
        match self {
            ElementProperty::Compressed(text) => ElementProperty::String(text.text()),
            ElementProperty::Deduped(text) => ElementProperty::String(text.as_str().into()),
            ElementProperty::Array(mut items) => {
                for item in items.iter_mut().filter(|item| item.holds_compressed()) {
                    *item = std::mem::replace(item, ElementProperty::Null).into_logical();
                }
                ElementProperty::Array(items)
            }
            ElementProperty::Object(mut map) => {
                map.values_mut().for_each(ElementProperty::make_logical);
                ElementProperty::Object(map)
            }
            ElementProperty::Vertex(mut vertex) => {
                vertex
                    .properties_mut()
                    .values_mut()
                    .for_each(ElementProperty::make_logical);
                ElementProperty::Vertex(vertex)
            }
            ElementProperty::Edge(mut edge) => {
                edge.properties_mut()
                    .values_mut()
                    .for_each(ElementProperty::make_logical);
                ElementProperty::Edge(edge)
            }
            ElementProperty::Map(map) => {
                if map
                    .iter()
                    .any(|(k, v)| k.holds_compressed() || v.holds_compressed())
                {
                    ElementProperty::from_entries(
                        map.into_iter()
                            .map(|(k, v)| (k.into_logical(), v.into_logical())),
                    )
                } else {
                    ElementProperty::Map(map)
                }
            }
            plain @ (ElementProperty::Null
            | ElementProperty::String(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_)) => plain,
        }
    }

    /// Decompresses every compressed string inside `self`, in place.
    pub(crate) fn make_logical(&mut self) {
        if self.holds_compressed() {
            *self = std::mem::replace(self, ElementProperty::Null).into_logical();
        }
    }

    /// Whether a storage-only value ([`ElementProperty::Compressed`] or
    /// [`ElementProperty::Deduped`]) is inside `self` (or is `self`).
    pub fn holds_compressed(&self) -> bool {
        self.holds_compressed_within(0)
    }

    /// [`Self::holds_compressed`] by recursion up to depth 32 (no allocation for the common,
    /// shallow value), by an explicit stack below that: a value may nest deeper than the
    /// thread's stack allows (see `value/depth.rs`).
    fn holds_compressed_within(&self, depth: usize) -> bool {
        const RECURSION_DEPTH: usize = 32;
        if depth >= RECURSION_DEPTH {
            return self.holds_compressed_deep();
        }
        match self {
            ElementProperty::Compressed(_) | ElementProperty::Deduped(_) => true,
            ElementProperty::Array(items) => items
                .iter()
                .any(|item| item.holds_compressed_within(depth + 1)),
            ElementProperty::Object(map) => map
                .values()
                .any(|value| value.holds_compressed_within(depth + 1)),
            ElementProperty::Vertex(vertex) => vertex
                .properties()
                .values()
                .any(|value| value.holds_compressed_within(depth + 1)),
            ElementProperty::Edge(edge) => edge
                .properties()
                .values()
                .any(|value| value.holds_compressed_within(depth + 1)),
            ElementProperty::Map(map) => map.iter().any(|(k, v)| {
                k.holds_compressed_within(depth + 1) || v.holds_compressed_within(depth + 1)
            }),
            ElementProperty::Null
            | ElementProperty::String(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_) => false,
        }
    }

    /// The explicit-stack walk of [`Self::holds_compressed_within`].
    #[inline(never)]
    fn holds_compressed_deep(&self) -> bool {
        let mut stack: Vec<&ElementProperty> = Vec::new();
        let mut current = self;
        loop {
            match current {
                ElementProperty::Compressed(_) | ElementProperty::Deduped(_) => return true,
                ElementProperty::Array(items) => stack.extend(items.iter()),
                ElementProperty::Object(map) => stack.extend(map.values()),
                ElementProperty::Vertex(vertex) => stack.extend(vertex.properties().values()),
                ElementProperty::Edge(edge) => stack.extend(edge.properties().values()),
                ElementProperty::Map(map) => {
                    for (key, value) in map.iter() {
                        stack.push(key);
                        stack.push(value);
                    }
                }
                ElementProperty::Null
                | ElementProperty::String(_)
                | ElementProperty::Number(_)
                | ElementProperty::Boolean(_)
                | ElementProperty::Uuid(_)
                | ElementProperty::Temporal(_)
                | ElementProperty::Vector(_) => {}
            }
            match stack.pop() {
                Some(next) => current = next,
                None => return false,
            }
        }
    }

    /// Calls `f` with the text of a string value (plain or compressed; a compressed one is
    /// decompressed into a thread-local buffer, nothing is allocated per call), `None` for any
    /// other value. The read for steps that only look at a string (predicates, regex, `has`).
    pub fn with_text<R>(&self, f: impl FnOnce(&str) -> R) -> Option<R> {
        match self {
            ElementProperty::String(text) => Some(f(text.as_str())),
            ElementProperty::Compressed(text) => Some(text.with_text(f)),
            ElementProperty::Deduped(text) => Some(f(text.as_str())),
            _ => None,
        }
    }

    /// Whether this is a string value, plain or compressed.
    pub fn is_text(&self) -> bool {
        matches!(
            self,
            ElementProperty::String(_)
                | ElementProperty::Compressed(_)
                | ElementProperty::Deduped(_)
        )
    }

    /// The text of a string value, plain or compressed (a compressed one is decompressed into
    /// an owned string); `None` for any other value.
    pub fn text(&self) -> Option<Cow<'_, str>> {
        match self {
            ElementProperty::String(text) => Some(Cow::Borrowed(text.as_str())),
            ElementProperty::Compressed(text) => Some(Cow::Owned(text.text().into_string())),
            ElementProperty::Deduped(text) => Some(Cow::Borrowed(text.as_str())),
            _ => None,
        }
    }
    /// Returns the kind of graph element (`"vertex"` or `"edge"`) this value holds at any depth
    /// of `Array`/`Object` nesting, or `None` when it holds none.
    ///
    /// Vertices and edges are stream values, not property values: storage rejects any value for
    /// which this returns `Some`.
    pub(crate) fn element_ref_kind(&self) -> Option<&'static str> {
        match self {
            ElementProperty::Vertex(_) => Some("vertex"),
            ElementProperty::Edge(_) => Some("edge"),
            ElementProperty::Map(_) => Some("map with a non-string key"),
            ElementProperty::Array(items) => items.iter().find_map(Self::element_ref_kind),
            ElementProperty::Object(map) => map.values().find_map(Self::element_ref_kind),
            ElementProperty::Null
            | ElementProperty::String(_)
            | ElementProperty::Compressed(_)
            | ElementProperty::Deduped(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_) => None,
        }
    }

    /// Builds a map value from entries, canonicalized: when every key is a string the result is an
    /// [`ElementProperty::Object`], otherwise an [`ElementProperty::Map`].
    pub fn from_entries(
        entries: impl IntoIterator<Item = (ElementProperty, ElementProperty)>,
    ) -> ElementProperty {
        entries.into_iter().collect::<ValueMap>().into_property()
    }

    /// Hashes a string key exactly as `ElementProperty::String(key)` hashes (used by the zero-allocation lookup).
    pub(crate) fn hash_string_key<H: std::hash::Hasher>(key: &str, state: &mut H) {
        use std::hash::Hash;
        state.write_u8(1);
        key.hash(state);
    }

    /// This value as an array, or `None` if it isn't one.
    pub fn as_array(&self) -> Option<&Vec<ElementProperty>> {
        match self {
            ElementProperty::Array(val) => Some(val),
            _ => None,
        }
    }

    /// This value as a string slice, or `None` if it isn't a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ElementProperty::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    /// Same as [`Self::as_str`].
    pub fn as_string(&self) -> Option<&str> {
        match self {
            ElementProperty::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    /// This value as a [`Number`]-holding `ElementProperty`, or `None` if it isn't numeric.
    pub fn as_number(&self) -> Option<&Self> {
        match self {
            ElementProperty::Number(_) => Some(self),
            _ => None,
        }
    }

    /// This value as a `bool`, or `None` if it isn't one.
    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            ElementProperty::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    /// This value as an `i64`, or `None` if it isn't a number stored as one.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ElementProperty::Number(value) => value.as_i64(),
            _ => None,
        }
    }

    /// This value as an `f64`, or `None` if it isn't a number stored as one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ElementProperty::Number(value) => value.as_f64(),
            _ => None,
        }
    }

    /// This value as a nested property map, or `None` if it isn't one.
    pub fn as_object(&self) -> Option<&PropertyMap> {
        match self {
            ElementProperty::Object(value) => Some(value),
            _ => None,
        }
    }

    /// The property entries of a map-shaped value: an object's entries, or a vertex's or an
    /// edge's properties (never their id, label or endpoints); `None` for any other value.
    pub fn map_entries(&self) -> Option<&PropertyMap> {
        match self {
            ElementProperty::Object(map) => Some(map),
            ElementProperty::Vertex(vertex) => Some(vertex.properties()),
            ElementProperty::Edge(edge) => Some(edge.properties()),
            _ => None,
        }
    }

    /// This value as a vertex, or `None` if it isn't a vertex.
    pub fn as_vertex(&self) -> Option<&VertexValue> {
        match self {
            ElementProperty::Vertex(value) => Some(value),
            _ => None,
        }
    }

    /// This value as an edge, or `None` if it isn't an edge.
    pub fn as_edge(&self) -> Option<&EdgeValue> {
        match self {
            ElementProperty::Edge(value) => Some(value),
            _ => None,
        }
    }

    /// This value as a date or time value, or `None` if it isn't one.
    pub fn as_temporal(&self) -> Option<Temporal> {
        match self {
            ElementProperty::Temporal(value) => Some(*value),
            _ => None,
        }
    }

    /// This value as a vector, or `None` if it isn't one.
    pub fn as_vector(&self) -> Option<&Vector> {
        match self {
            ElementProperty::Vector(value) => Some(value),
            _ => None,
        }
    }

    /// This value as raw UUID bits, or `None` if it isn't a UUID.
    pub fn as_uuid(&self) -> Option<u128> {
        match self {
            ElementProperty::Uuid(value) => Some(*value),
            _ => None,
        }
    }

    /// Estimated heap footprint of this value's owned indirect data, in bytes. `0` for every
    /// inline/`Copy` variant (`Null`, `Number`, `Boolean`, `Uuid`); for `String`, only the bytes
    /// actually heap-allocated by `CompactString` (its own inline-vs-heap threshold is respected,
    /// so a short string correctly contributes `0`); for `Array`, the `Vec`'s `.capacity()` times
    /// the value size; for `Object`/`Map`/`Vertex`/`Edge`, the box, the map's entries and hash
    /// index sized from its `.capacity()` (`property_map::index_map_heap_bytes`), heap-allocated
    /// ids and labels, plus a recursive sum over every element. A `Vector` reports its shared
    /// allocation in every holder; a compressed string its box and stored block.
    ///
    /// This is a `size_of`/`capacity`-based estimate, not a true allocator measurement — see
    /// [`crate::graph::MemoryStats`] and `MemorySource::Estimated` for the accuracy caveat. It is
    /// the single estimator shared by [`crate::graph::GraphStorage::memory_usage`]'s property walk
    /// and `.profile()`'s per-step `bytes_materialized` figure (design decision D1/D3 of the
    /// memory-inspection spec): one estimator, two callers, so the two numbers stay consistent
    /// with each other.
    pub fn estimated_heap_bytes(&self) -> u64 {
        match self {
            ElementProperty::Null
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_) => 0,
            // The components exactly, plus the `Arc` allocation's header.
            ElementProperty::Vector(vector) => vector.heap_bytes(),
            ElementProperty::String(value) => compact_string_heap_bytes(value),
            // The stored (compressed) size; the dictionary is shared and counted per rule.
            ElementProperty::Compressed(text) => text.heap_bytes(),
            // Shared: the text is counted once, in its rule's table (`MemoryStats`).
            ElementProperty::Deduped(_) => 0,
            ElementProperty::Array(values) => {
                let container =
                    values.capacity() as u64 * std::mem::size_of::<ElementProperty>() as u64;
                container
                    + values
                        .iter()
                        .map(ElementProperty::estimated_heap_bytes)
                        .sum::<u64>()
            }
            // The box holds the map's inline part; its entries and index are heap of their own.
            ElementProperty::Object(element) => {
                std::mem::size_of::<PropertyMap>() as u64 + estimated_element_heap_bytes(element)
            }
            ElementProperty::Vertex(vertex) => {
                std::mem::size_of::<element_value::VertexValue>() as u64 + vertex.heap_bytes()
            }
            ElementProperty::Edge(edge) => {
                std::mem::size_of::<element_value::EdgeValue>() as u64 + edge.heap_bytes()
            }
            ElementProperty::Map(map) => {
                std::mem::size_of::<value_map::ValueMap>() as u64
                    + map.table_heap_bytes()
                    + map
                        .iter()
                        .map(|(k, v)| k.estimated_heap_bytes() + v.estimated_heap_bytes())
                        .sum::<u64>()
            }
        }
    }
}

/// Canonical type rank used to order [`ElementProperty`] values of *different* variants, when
/// [`compare_element_property`] has no more specific rule for the pair (see that function's doc
/// comment for the full picture).
///
/// Real Apache TinkerPop natural ordering (`Order.asc`/`Order.desc`) is `Comparator<Object>`
/// wrapping `Comparable::compareTo`: ordering a stream that mixes, say, a `String` and an
/// `Integer` property throws a `ClassCastException` at runtime (confirmed against the TinkerPop
/// 3.7 `Order` API docs and reported `gremlin-users` issues — TinkerPop does not define a
/// graceful cross-type order, it fails). This crate cannot do that here: `compare_element_property`
/// returns a plain [`std::cmp::Ordering`], not a `Result`, and CLAUDE.md rule 1 forbids panicking
/// on arbitrary user data on any step/script path, which the future `order()` step (parts 2/3 of
/// this series) is. So, for genuinely cross-type comparisons only, values fall back to this fixed,
/// stable rank instead of raising an error — the sort completes deterministically on heterogeneous
/// input. This mirrors the fallback other property-graph/document engines take when they *do*
/// define full cross-type "orderability" instead of mirroring Java's exception (e.g. BSON/
/// MongoDB's canonical type-sort order, openCypher's type-based comparability rules).
///
/// Elements rank `Vertex` before `Edge`; two of the same kind are ordered by id (see
/// `compare_elements_by_id`), not by rank.
///
/// Every variant is listed explicitly (no wildcard arm) so a future new variant fails to compile
/// here until someone picks its rank on purpose, rather than silently defaulting to some other
/// variant's position.
//
// Wired into the `order()` step (`traversal/step/flatbarrier_steps/step_order.rs`, part 2 of this
// series) via `compare_element_property` below.
fn type_rank(value: &ElementProperty) -> u8 {
    match value {
        ElementProperty::Null => 0,
        ElementProperty::Boolean(_) => 1,
        ElementProperty::Number(_) => 2,
        // TinkerPop orderability: dates after numbers, before strings; the temporal kinds order
        // among themselves by `Temporal`'s own `Ord` (date, localdatetime, datetime, time,
        // duration).
        ElementProperty::Temporal(_) => 3,
        // Vectors (a Graphersal extension, TinkerPop has none) after the dates (duration last
        // among them), before strings; among themselves by `Vector`'s own `Ord`.
        ElementProperty::Vector(_) => 4,
        // A compressed string is the string it holds.
        ElementProperty::String(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => 5,
        ElementProperty::Uuid(_) => 6,
        ElementProperty::Array(_) => 7,
        ElementProperty::Object(_) => 8,
        ElementProperty::Map(_) => 9,
        ElementProperty::Vertex(_) => 10,
        ElementProperty::Edge(_) => 11,
    }
}

/// Structural comparison of two [`PropertyMap`] property maps, shared by [`compare_element_property`]'s
/// `Object`/`Vertex`/`Edge` arms: entries are compared pairwise in the maps' own (insertion)
/// order, key first (lexicographically) then value (recursively via `compare_element_property`),
/// and a map that runs out of entries first while otherwise agreeing on the common prefix sorts
/// before the longer one. This is a defined, stable, total order over `PropertyMap`, not a claim that
/// it is a semantically meaningful one (`order()` is expected to run on scalar projections, not
/// whole vertices/edges/objects) — see [`compare_element_property`].
fn compare_elements(a: &PropertyMap, b: &PropertyMap) -> std::cmp::Ordering {
    let mut left = a.iter();
    let mut right = b.iter();
    loop {
        return match (left.next(), right.next()) {
            (Some((a_key, a_value)), Some((b_key, b_value))) => {
                match a_key.as_str().cmp(b_key.as_str()) {
                    std::cmp::Ordering::Equal => match compare_element_property(a_value, b_value) {
                        std::cmp::Ordering::Equal => continue,
                        other => other,
                    },
                    other => other,
                }
            }
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => std::cmp::Ordering::Equal,
        };
    }
}

/// Orders two vertices or two edges the way TinkerPop does, by id (strings as strings, never
/// parsed into numbers, rule 7); only when the ids are equal do the properties decide
/// (structurally, [`compare_elements`]).
///
/// **Deviation from TinkerPop 3.8.2** (user's guide: "Predicates: Comparison and Resolution"):
/// ids are stored as strings, so they order lexicographically (`"10"` before `"9"`); TinkerPop's
/// numeric ids order numerically.
fn compare_elements_by_id(
    (a_id, a): (&str, &PropertyMap),
    (b_id, b): (&str, &PropertyMap),
) -> std::cmp::Ordering {
    match a_id.cmp(b_id) {
        std::cmp::Ordering::Equal => compare_elements(a, b),
        other => other,
    }
}

/// Total-order comparison across [`ElementProperty`] values, the shared comparator the `order()`
/// step (`traversal/step/flatbarrier_steps/step_order.rs`) sorts traversers with. Every variant
/// is matched explicitly; nothing silently collapses to `Equal` for a pair nobody thought about.
///
/// - `Number` values compare numerically across the `NegInt`/`Float` sub-types by reusing
///   [`Number`]'s own `Ord` impl, so an integer and a float of equal value compare `Equal` (e.g.
///   `1` and `1.0`).
/// - `String` values compare lexicographically via `str`'s `Ord` (plain byte-wise order, no
///   locale/collation — matches Java `String.compareTo`'s code-point-ish behavior closely enough
///   for this crate's purposes, and CLAUDE.md rule 7 already forbids content-sniffing a string
///   into some other logical type before comparing it).
/// - `Boolean` values order `false < true` (`bool`'s own `Ord`).
/// - `Null` compares `Equal` to `Null`; `Uuid` compares as its raw `u128`.
/// - `Array` values compare element-wise (via this same function, recursively), then by length
///   when one is a prefix of the other. Two `Object`s compare structurally via
///   [`compare_elements`]; two `Vertex`es or two `Edge`s compare by their `id` first (as
///   TinkerPop orders elements), falling back to the structural comparison only for a missing
///   or equal id. A `Vertex` ranks before an `Edge`.
/// - Two values of **different** variants (including two structurally-different `Array`s that
///   still need a decision after the element-wise walk — handled above, not here) fall back to
///   [`type_rank`]'s fixed type order. This is the one place real TinkerPop natural ordering
///   simply throws (`ClassCastException`, see `type_rank`'s doc comment); this function instead
///   returns a defined, stable `Ordering` and never panics, so a sort over heterogeneous input
///   always completes.
pub(crate) fn compare_element_property(
    a: &ElementProperty,
    b: &ElementProperty,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (ElementProperty::Null, ElementProperty::Null) => Ordering::Equal,
        (ElementProperty::Boolean(a), ElementProperty::Boolean(b)) => a.cmp(b),
        (ElementProperty::Number(a), ElementProperty::Number(b)) => a.cmp(b),
        (ElementProperty::String(a), ElementProperty::String(b)) => a.as_str().cmp(b.as_str()),
        // A compressed string orders as the string it holds.
        (ElementProperty::Compressed(_) | ElementProperty::Deduped(_), b) if b.is_text() => {
            compare_text(a, b)
        }
        (a, ElementProperty::Compressed(_) | ElementProperty::Deduped(_)) if a.is_text() => {
            compare_text(a, b)
        }
        (ElementProperty::Uuid(a), ElementProperty::Uuid(b)) => a.cmp(b),
        // Different temporal kinds order by kind (date, localdatetime, datetime, time,
        // duration), then by value; a datetime by its instant.
        (ElementProperty::Temporal(a), ElementProperty::Temporal(b)) => a.cmp(b),
        // By kind, dimension, then the components.
        (ElementProperty::Vector(a), ElementProperty::Vector(b)) => a.cmp(b),
        (ElementProperty::Array(a), ElementProperty::Array(b)) => {
            let mut left = a.iter();
            let mut right = b.iter();
            loop {
                return match (left.next(), right.next()) {
                    (Some(a_value), Some(b_value)) => {
                        match compare_element_property(a_value, b_value) {
                            Ordering::Equal => continue,
                            other => other,
                        }
                    }
                    (Some(_), None) => Ordering::Greater,
                    (None, Some(_)) => Ordering::Less,
                    (None, None) => Ordering::Equal,
                };
            }
        }
        (ElementProperty::Object(a), ElementProperty::Object(b)) => compare_elements(a, b),
        (ElementProperty::Map(a), ElementProperty::Map(b)) => {
            // Entry-wise in insertion order, key first, then value; the shorter prefix sorts first.
            let mut left = a.iter();
            let mut right = b.iter();
            loop {
                return match (left.next(), right.next()) {
                    (Some((a_key, a_value)), Some((b_key, b_value))) => {
                        match compare_element_property(a_key, b_key) {
                            Ordering::Equal => match compare_element_property(a_value, b_value) {
                                Ordering::Equal => continue,
                                other => other,
                            },
                            other => other,
                        }
                    }
                    (Some(_), None) => Ordering::Greater,
                    (None, Some(_)) => Ordering::Less,
                    (None, None) => Ordering::Equal,
                };
            }
        }
        (ElementProperty::Vertex(a), ElementProperty::Vertex(b)) => {
            compare_elements_by_id((a.id(), a.properties()), (b.id(), b.properties()))
        }
        (ElementProperty::Edge(a), ElementProperty::Edge(b)) => {
            compare_elements_by_id((a.id(), a.properties()), (b.id(), b.properties()))
        }
        (a, b) => type_rank(a).cmp(&type_rank(b)),
    }
}

/// A logical copy of a stored property map: every compressed string decompressed (D5).
pub(crate) fn logical_element(element: &PropertyMap) -> PropertyMap {
    // A stored map keeps its shared key list: the copy is the shape (one `Arc`) and the logical
    // values, with no key hashed and no `IndexMap` built (the hot path of materializing elements).
    if let Some((shape, values)) = element.shaped_parts() {
        return PropertyMap::from_shaped(
            std::sync::Arc::clone(shape),
            values.iter().map(ElementProperty::to_logical).collect(),
        );
    }
    element
        .iter()
        .map(|(key, value)| (key.clone(), value.to_logical()))
        .collect()
}

/// Estimated heap footprint of a [`PropertyMap`] (a vertex/edge property map): the map's own
/// storage ([`PropertyMap::own_heap_bytes`]: for a free-standing map its two allocations, its
/// entries and its hash index, sized from its `.capacity()` by
/// [`property_map::index_map_heap_bytes`], plus each key's heap bytes; for a map stored in a
/// graph its value slice, the shared key list being counted once by the shape table) and each
/// value's [`ElementProperty::estimated_heap_bytes`]. Shared by
/// [`crate::graph::GraphStorage::memory_usage`]'s `property_bytes` walk and `.profile()`'s per-step
/// estimation (D1/D3).
pub(crate) fn estimated_element_heap_bytes(element: &PropertyMap) -> u64 {
    element.own_heap_bytes()
        + element
            .values()
            .map(ElementProperty::estimated_heap_bytes)
            .sum::<u64>()
}

/// The heap bytes of a [`CompactString`]: its capacity when it spilled to the heap, `0` when it
/// is stored inline.
pub(crate) fn compact_string_heap_bytes(text: &CompactString) -> u64 {
    if text.is_heap_allocated() {
        text.capacity() as u64
    } else {
        0
    }
}

impl fmt::Debug for ElementProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElementProperty::Null => write!(f, "null"),
            ElementProperty::String(value) => write!(f, "\"{}\"", value),
            ElementProperty::Object(obj) => write!(f, "{:?}", obj),
            ElementProperty::Map(map) => write!(f, "{:?}", map),
            ElementProperty::Vertex(vertex) => write!(f, "v[{}]", vertex.id()),
            ElementProperty::Edge(edge) => write!(
                f,
                "e[{}][{}-{}->{}]",
                edge.id(),
                edge.out_v().id(),
                edge.label().unwrap_or(""),
                edge.in_v().id()
            ),
            ElementProperty::Array(array) => write!(f, "{:?}", array),
            ElementProperty::Number(value) => write!(f, "{}", value),
            ElementProperty::Boolean(value) => write!(f, "{}", value),
            ElementProperty::Uuid(value) => write!(f, "{}", uuid::format_canonical(*value)),
            ElementProperty::Temporal(value) => write!(f, "{value}"),
            ElementProperty::Vector(value) => write!(f, "{value}"),
            ElementProperty::Compressed(text) => write!(f, "{text:?}"),
            ElementProperty::Deduped(text) => write!(f, "{text:?}"),
        }
    }
}

impl From<&str> for ElementProperty {
    fn from(value: &str) -> Self {
        ElementProperty::String(CompactString::new(value))
    }
}

impl From<&&str> for ElementProperty {
    fn from(value: &&str) -> Self {
        ElementProperty::String(CompactString::new(*value))
    }
}

impl From<String> for ElementProperty {
    fn from(value: String) -> Self {
        ElementProperty::String(CompactString::new(value))
    }
}

impl From<&String> for ElementProperty {
    fn from(value: &String) -> Self {
        ElementProperty::String(CompactString::new(value))
    }
}

impl From<CompactString> for ElementProperty {
    fn from(value: CompactString) -> Self {
        ElementProperty::String(value)
    }
}

impl From<&CompactString> for ElementProperty {
    fn from(value: &CompactString) -> Self {
        ElementProperty::String(value.clone())
    }
}

impl<'a> From<Cow<'a, str>> for ElementProperty {
    fn from(value: Cow<'a, str>) -> Self {
        ElementProperty::String(CompactString::new(value))
    }
}

impl From<usize> for ElementProperty {
    fn from(value: usize) -> Self {
        ElementProperty::Number(value.into())
    }
}

impl From<&usize> for ElementProperty {
    fn from(value: &usize) -> Self {
        ElementProperty::Number((*value).into())
    }
}

impl From<Vec<usize>> for ElementProperty {
    fn from(value: Vec<usize>) -> Self {
        ElementProperty::Array(value.iter().map(|r| r.into()).collect())
    }
}

impl From<i32> for ElementProperty {
    fn from(value: i32) -> Self {
        ElementProperty::Number(value.into())
    }
}

impl From<&i32> for ElementProperty {
    fn from(value: &i32) -> Self {
        ElementProperty::Number((*value).into())
    }
}

impl From<Vec<i32>> for ElementProperty {
    fn from(value: Vec<i32>) -> Self {
        ElementProperty::Array(value.iter().map(|r| r.into()).collect())
    }
}

impl From<i64> for ElementProperty {
    fn from(value: i64) -> Self {
        ElementProperty::Number(value.into())
    }
}

impl From<&i64> for ElementProperty {
    fn from(value: &i64) -> Self {
        ElementProperty::Number((*value).into())
    }
}

impl From<Vec<i64>> for ElementProperty {
    fn from(value: Vec<i64>) -> Self {
        ElementProperty::Array(value.iter().map(|r| r.into()).collect())
    }
}

impl From<f64> for ElementProperty {
    fn from(value: f64) -> Self {
        ElementProperty::Number(value.into())
    }
}

impl From<&f64> for ElementProperty {
    fn from(value: &f64) -> Self {
        ElementProperty::Number((*value).into())
    }
}

impl From<Vec<f64>> for ElementProperty {
    fn from(value: Vec<f64>) -> Self {
        ElementProperty::Array(value.iter().map(|r| r.into()).collect())
    }
}

impl From<Vec<&str>> for ElementProperty {
    fn from(value: Vec<&str>) -> Self {
        ElementProperty::Array(value.iter().map(|r| r.into()).collect())
    }
}

impl From<bool> for ElementProperty {
    fn from(value: bool) -> Self {
        ElementProperty::Boolean(value)
    }
}

impl From<Vector> for ElementProperty {
    fn from(value: Vector) -> Self {
        ElementProperty::Vector(value)
    }
}

impl From<Temporal> for ElementProperty {
    fn from(value: Temporal) -> Self {
        ElementProperty::Temporal(value)
    }
}

macro_rules! temporal_property {
    ($($ty:ident),*) => {$(
        impl From<temporal::$ty> for ElementProperty {
            fn from(value: temporal::$ty) -> Self {
                ElementProperty::Temporal(value.into())
            }
        }
    )*};
}

temporal_property!(Date, Time, LocalDateTime, DateTime, Duration);

/// A schema as a value: its canonical format ([`GraphSchema::to_value`]), the one data shape
/// of a schema everywhere.
impl From<&GraphSchema> for ElementProperty {
    fn from(schema: &GraphSchema) -> Self {
        schema.to_value()
    }
}

/// See the `&GraphSchema` conversion.
impl From<GraphSchema> for ElementProperty {
    fn from(schema: GraphSchema) -> Self {
        schema.to_value()
    }
}

impl std::hash::Hash for ElementProperty {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // One explicit tag byte per variant: `ValueMap::get_str` hashes a `&str` exactly like `String`.
        match self {
            ElementProperty::Null => state.write_u8(0),
            ElementProperty::String(val) => Self::hash_string_key(val, state),
            // Hashes as the string it holds (logical equality).
            ElementProperty::Compressed(val) => hash_compressed(val, state),
            ElementProperty::Deduped(val) => Self::hash_string_key(val.as_str(), state),
            ElementProperty::Number(val) => {
                state.write_u8(2);
                val.hash(state);
            }
            ElementProperty::Boolean(val) => {
                state.write_u8(3);
                val.hash(state);
            }
            ElementProperty::Uuid(val) => {
                state.write_u8(4);
                val.hash(state);
            }
            // Equality of maps is order-independent, so the hash is too.
            ElementProperty::Object(val) => {
                state.write_u8(5);
                value_map::hash_entries_unordered(val.iter(), state);
            }
            ElementProperty::Array(val) => {
                state.write_u8(6);
                val.hash(state);
            }
            ElementProperty::Vertex(val) => {
                state.write_u8(7);
                val.id().hash(state);
                value_map::hash_entries_unordered(val.properties().iter(), state);
            }
            ElementProperty::Edge(val) => {
                state.write_u8(8);
                val.id().hash(state);
                value_map::hash_entries_unordered(val.properties().iter(), state);
            }
            ElementProperty::Map(val) => {
                state.write_u8(9);
                val.hash(state);
            }
            // `Temporal`'s own hash agrees with its equality (a datetime hashes its instant).
            ElementProperty::Temporal(val) => {
                state.write_u8(10);
                val.hash(state);
            }
            // `Vector`'s own hash agrees with its equality (`-0.0` hashes as `0.0`).
            ElementProperty::Vector(val) => {
                state.write_u8(11);
                val.hash(state);
            }
        }
    }
}

/// What a lazy property handle ([`TraverserValue::VertexProperty`]/[`TraverserValue::EdgeProperty`])
/// stands for.
///
/// `Element` and `Value` read the same stored property; they differ in what the handle *is* to the
/// steps that treat it as an object rather than as its value (`dedup()`, `as_string()`, set side
/// effects), and equality ignores the difference: two handles of one (key, element) are equal
/// either way. `Id`, `Label` and `Labels` read the element's id, label or label set instead of a
/// property (the handle's key is then only their display name), so a property named `id` or
/// `label` never stands in for them, nor they for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PropertyHandleKind {
    /// A TinkerPop property element, as `properties()` yields it: it has an identity (its
    /// element and key), a `toString` form (`vp[name->marko]`) and is not its value.
    Element,
    /// A property value read lazily, as `values()` yields it. It behaves as the plain value it
    /// will be read as; the handle only defers the read.
    Value,
    /// The element's id read lazily, as `id()` yields it. Never a property: a property named
    /// `id` is a [`PropertyHandleKind::Value`] handle and reads the property.
    Id,
    /// The element's (primary) label read lazily, as `label()` yields it. Never a property: a
    /// property named `label` is a [`PropertyHandleKind::Value`] handle and reads the property.
    Label,
    /// The element's label set read lazily, as `labels()` yields it. Never a property.
    Labels,
}

impl PropertyHandleKind {
    /// Whether the handle reads a stored property (`Element`, `Value`) rather than the element's
    /// id, label or label set (`Id`, `Label`, `Labels`).
    pub fn reads_property(self) -> bool {
        match self {
            PropertyHandleKind::Element | PropertyHandleKind::Value => true,
            PropertyHandleKind::Id | PropertyHandleKind::Label | PropertyHandleKind::Labels => {
                false
            }
        }
    }

    /// Whether the handle stands for the plain value it reads (every kind but `Element`).
    pub fn is_value(self) -> bool {
        !matches!(self, PropertyHandleKind::Element)
    }

    /// What the handle reads, for equality and hashing: `Element` and `Value` read the same
    /// stored property and compare equal; `Id`, `Label` and `Labels` each read something else.
    pub(crate) fn reading(self) -> u8 {
        match self {
            PropertyHandleKind::Element | PropertyHandleKind::Value => 0,
            PropertyHandleKind::Id => 1,
            PropertyHandleKind::Label => 2,
            PropertyHandleKind::Labels => 3,
        }
    }
}

/// Values carried by `Traverser` items during step evaluation.
#[non_exhaustive]
pub enum TraverserValue<S: GraphStorage> {
    /// A materialized scalar/array/map value, optionally tagged with the vertex it came from.
    Computed(ElementProperty, Option<S::VertexId>),
    /// A lazy handle to one vertex property, not yet read. The [`PropertyHandleKind`] tells a
    /// TinkerPop `VertexProperty` element (`properties()`) from a lazily read value (`values()`).
    VertexProperty(CompactString, S::VertexId, PropertyHandleKind),
    /// A lazy handle to a vertex, not yet materialized.
    Vertex(S::VertexId),
    /// A lazy handle to an edge, not yet materialized.
    Edge(S::EdgeId),
    /// A lazy handle to one edge property, not yet read. See [`TraverserValue::VertexProperty`]
    /// for the [`PropertyHandleKind`].
    EdgeProperty(CompactString, S::EdgeId, PropertyHandleKind),
    /// A property element addressed by a `jpath(..)` key, as `properties(jpath("a.b[1]"))` yields it.
    ///
    /// A path has no stored property to point at, so this handle is materialized: the canonical
    /// path text (`$.a.b[1]`, what `key()` returns) and the value the path ended on (what
    /// `value()` returns). It is a Graphersal extension (a plain name never produces it); it
    /// behaves like a property element otherwise: `label()`/`hasLabel()` see the key, `id()` is
    /// an error, `dedup()` compares key and value, a terminal materializes as the value. Boxed so
    /// the variant stays one pointer wide.
    PathProperty(Box<(CompactString, ElementProperty)>),
    /// The path a traverser took through the graph (`path()`).
    Path(TraverserPath<S>),
    /// Lazy structured record with a shared static key schema.
    Record {
        /// The record's keys, shared across every row of the same shape: names, and the tokens
        /// `T.id`/`T.label`/`Direction.OUT`/`Direction.IN` of `elementMap()`/`valueMap(true)`.
        keys: Arc<Vec<RecordKey>>,
        /// The record's values, positionally aligned with `keys`.
        values: Vec<TraverserValue<S>>,
    },
    /// A map produced by script evaluation (e.g. `group()`).
    Object(StreamMap<S>),
    /// One `key -> value` entry of a map, TinkerPop's `Map.Entry`.
    ///
    /// Produced by `unfold()` on a map (`Object`/`Record`) and read by `select(Column::Keys)`/
    /// `select(Column::Values)`, which return the scalar key/value of an entry but a list of all
    /// keys/values of a whole map. Keeping the entry a distinct kind (rather than a single-key
    /// `Object`) is what lets those two cases be told apart: a one-group `group()` result is a
    /// map, not an entry. Boxed so the variant stays one pointer wide. It materializes as a
    /// single-key map (`{key: value}`), the way the Gremlin console prints `marko=3`.
    Entry(Box<(TraverserValue<S>, TraverserValue<S>)>),
    /// A list of values.
    Array(Vec<TraverserValue<S>>),
    /// A graph schema (e.g. the result of `g.schema()`).
    Schema(Box<GraphSchema>),
    /// A graph value: the snapshot `cap("sg")` returns for `subgraph("sg")`. It is a shared handle
    /// ([`Graph`](crate::graph::Graph), the type a script session binds as `g`), equal only to the
    /// same handle, and materializes as `{"vertices": [..], "edges": [..]}`.
    Graph(Arc<crate::graph::Graph>),
}

impl<S: GraphStorage> std::fmt::Debug for TraverserValue<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraverserValue::Computed(value, id) => {
                f.debug_tuple("Computed").field(value).field(id).finish()
            }
            TraverserValue::VertexProperty(prop, id, _) => f
                .debug_tuple("VertexProperty")
                .field(prop)
                .field(id)
                .finish(),
            TraverserValue::Vertex(id) => f.debug_tuple("Vertex").field(id).finish(),
            TraverserValue::Edge(id) => f.debug_tuple("Edge").field(id).finish(),
            TraverserValue::EdgeProperty(prop, id, _) => {
                f.debug_tuple("EdgeProperty").field(prop).field(id).finish()
            }
            TraverserValue::Path(path) => f.debug_tuple("Path").field(path).finish(),
            TraverserValue::Record { keys, values } => f
                .debug_struct("Record")
                .field("keys", keys)
                .field("values", values)
                .finish(),
            TraverserValue::Object(obj) => f.debug_tuple("Object").field(obj).finish(),
            TraverserValue::PathProperty(prop) => f
                .debug_tuple("PathProperty")
                .field(&prop.0)
                .field(&prop.1)
                .finish(),
            TraverserValue::Entry(entry) => f
                .debug_tuple("Entry")
                .field(&entry.0)
                .field(&entry.1)
                .finish(),
            TraverserValue::Array(arr) => f.debug_tuple("Array").field(arr).finish(),
            TraverserValue::Schema(schema) => f.debug_tuple("Schema").field(schema).finish(),
            TraverserValue::Graph(graph) => {
                let graph = graph.read();
                f.debug_struct("Graph")
                    .field("vertices", &graph.vertex_count())
                    .field("edges", &graph.edge_count())
                    .finish()
            }
        }
    }
}

impl<S: GraphStorage> Clone for TraverserValue<S> {
    fn clone(&self) -> Self {
        match self {
            TraverserValue::Computed(value, id) => TraverserValue::Computed(value.clone(), *id),
            TraverserValue::VertexProperty(prop, id, kind) => {
                TraverserValue::VertexProperty(prop.clone(), *id, *kind)
            }
            TraverserValue::Vertex(id) => TraverserValue::Vertex(*id),
            TraverserValue::Edge(id) => TraverserValue::Edge(*id),
            TraverserValue::EdgeProperty(prop, id, kind) => {
                TraverserValue::EdgeProperty(prop.clone(), *id, *kind)
            }
            TraverserValue::Path(path) => TraverserValue::Path(path.clone()),
            TraverserValue::Record { keys, values } => TraverserValue::Record {
                keys: keys.clone(),
                values: values.clone(),
            },
            TraverserValue::Object(obj) => TraverserValue::Object(obj.clone()),
            TraverserValue::PathProperty(prop) => TraverserValue::PathProperty(prop.clone()),
            TraverserValue::Entry(entry) => TraverserValue::Entry(entry.clone()),
            TraverserValue::Array(arr) => TraverserValue::Array(arr.clone()),
            TraverserValue::Schema(schema) => TraverserValue::Schema(schema.clone()),
            TraverserValue::Graph(graph) => TraverserValue::Graph(graph.clone()),
        }
    }
}

impl<S: GraphStorage> std::cmp::Eq for TraverserValue<S> {}

impl<S: GraphStorage> PartialEq<TraverserValue<S>> for TraverserValue<S> {
    fn eq(&self, other: &TraverserValue<S>) -> bool {
        match (self, other) {
            (Self::Computed(l0, _), Self::Computed(r0, _)) => l0 == r0,
            (Self::VertexProperty(l0, l1, l2), Self::VertexProperty(r0, r1, r2)) => {
                l0 == r0 && l1 == r1 && l2.reading() == r2.reading()
            }
            (Self::Vertex(l0), Self::Vertex(r0)) => l0 == r0,
            (Self::Edge(l0), Self::Edge(r0)) => l0 == r0,
            (Self::EdgeProperty(l0, l1, l2), Self::EdgeProperty(r0, r1, r2)) => {
                l0 == r0 && l1 == r1 && l2.reading() == r2.reading()
            }
            (Self::Path(l0), Self::Path(r0)) => l0 == r0,
            (Self::Object(l0), Self::Object(r0)) => l0 == r0,
            (Self::PathProperty(l0), Self::PathProperty(r0)) => l0 == r0,
            (Self::Entry(l0), Self::Entry(r0)) => l0 == r0,
            (Self::Array(l0), Self::Array(r0)) => l0 == r0,
            (Self::Schema(l0), Self::Schema(r0)) => l0 == r0,
            (Self::Graph(l0), Self::Graph(r0)) => Arc::ptr_eq(l0, r0),
            (
                Self::Record {
                    keys: l_keys,
                    values: l_vals,
                },
                Self::Record {
                    keys: r_keys,
                    values: r_vals,
                },
            ) => {
                if l_vals.len() != r_vals.len() {
                    return false;
                }
                if Arc::ptr_eq(l_keys, r_keys) || l_keys == r_keys {
                    l_vals == r_vals
                } else {
                    for (lk, lv) in l_keys.iter().zip(l_vals.iter()) {
                        if let Some(r_idx) = r_keys.iter().position(|rk| rk == lk) {
                            if &r_vals[r_idx] != lv {
                                return false;
                            }
                        } else {
                            return false;
                        }
                    }
                    true
                }
            }
            (Self::Record { keys, values }, Self::Object(obj))
            | (Self::Object(obj), Self::Record { keys, values }) => {
                if keys.len() != obj.len() {
                    return false;
                }
                for (k, v) in keys.iter().zip(values.iter()) {
                    let key_tv =
                        TraverserValue::Computed(ElementProperty::String(k.to_compact()), None);
                    match obj.get(&key_tv) {
                        Some(obj_val) => {
                            if obj_val != v {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
                true
            }
            _ => false,
        }
    }
}

impl<S: GraphStorage> std::hash::Hash for TraverserValue<S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            TraverserValue::Computed(value, _) => value.hash(state),
            TraverserValue::VertexProperty(property, node_index, kind) => {
                property.hash(state);
                node_index.hash(state);
                kind.reading().hash(state);
            }
            TraverserValue::Vertex(vertex) => vertex.hash(state),
            TraverserValue::Edge(edge) => edge.hash(state),
            TraverserValue::EdgeProperty(property, edge_index, kind) => {
                property.hash(state);
                edge_index.hash(state);
                kind.reading().hash(state);
            }
            TraverserValue::Path(path) => path.hash(state),
            TraverserValue::Object(obj) => value_map::hash_entries_unordered(obj.iter(), state),
            TraverserValue::Record { keys, values } => {
                for (k, v) in keys.iter().zip(values.iter()) {
                    k.hash(state);
                    v.hash(state);
                }
            }
            TraverserValue::PathProperty(prop) => {
                prop.0.hash(state);
                prop.1.hash(state);
            }
            TraverserValue::Entry(entry) => {
                entry.0.hash(state);
                entry.1.hash(state);
            }
            TraverserValue::Array(arr) => arr.hash(state),
            TraverserValue::Schema(schema) => {
                schema.meta.is_some().hash(state);
                schema.mode.hash(state);
                schema.vertices.len().hash(state);
                schema.edges.len().hash(state);
            }
            TraverserValue::Graph(graph) => Arc::as_ptr(graph).hash(state),
        }
    }
}

impl<S: GraphStorage> From<&ElementProperty> for TraverserValue<S> {
    fn from(value: &ElementProperty) -> Self {
        match value {
            ElementProperty::Null => TraverserValue::Computed(value.clone(), None),
            ElementProperty::String(_) => TraverserValue::Computed(value.clone(), None),
            ElementProperty::Number(_) => TraverserValue::Computed(value.clone(), None),
            ElementProperty::Boolean(_) => TraverserValue::Computed(value.clone(), None),
            ElementProperty::Uuid(_) => TraverserValue::Computed(value.clone(), None),
            ElementProperty::Temporal(_) => TraverserValue::Computed(value.clone(), None),
            ElementProperty::Vector(_) => TraverserValue::Computed(value.clone(), None),
            // Boundary (D5): a stream value is always plain.
            ElementProperty::Compressed(text) => {
                TraverserValue::Computed(ElementProperty::String(text.text()), None)
            }
            ElementProperty::Deduped(text) => {
                TraverserValue::Computed(ElementProperty::String(text.as_str().into()), None)
            }
            ElementProperty::Array(value) => {
                TraverserValue::Array(value.iter().map(|r| r.into()).collect())
            }
            ElementProperty::Object(obj) => TraverserValue::Object(
                obj.iter()
                    .map(|(k, v)| (TraverserValue::from(k.clone()), TraverserValue::from(v)))
                    .collect(),
            ),
            // A vertex or an edge value stays one value (an element, not a map): its properties
            // are read through it (`values()`, `select()`), its id and label through the tokens.
            ElementProperty::Vertex(_) | ElementProperty::Edge(_) => {
                TraverserValue::Computed(value.to_logical(), None)
            }
            // A map keeps its typed keys on the stream.
            ElementProperty::Map(map) => TraverserValue::Object(
                map.iter()
                    .map(|(k, v)| (TraverserValue::from(k), TraverserValue::from(v)))
                    .collect(),
            ),
        }
    }
}

impl<S: GraphStorage> From<ElementProperty> for TraverserValue<S> {
    fn from(value: ElementProperty) -> Self {
        match value {
            ElementProperty::Null
            | ElementProperty::String(_)
            | ElementProperty::Number(_)
            | ElementProperty::Boolean(_)
            | ElementProperty::Uuid(_)
            | ElementProperty::Temporal(_)
            | ElementProperty::Vector(_) => TraverserValue::Computed(value, None),
            // Boundary (D5): a stream value is always plain.
            ElementProperty::Compressed(text) => {
                TraverserValue::Computed(ElementProperty::String(text.text()), None)
            }
            ElementProperty::Deduped(text) => {
                TraverserValue::Computed(ElementProperty::String(text.as_str().into()), None)
            }
            ElementProperty::Array(array) => {
                TraverserValue::Array(array.into_iter().map(|r| r.into()).collect())
            }
            ElementProperty::Object(obj) => TraverserValue::Object(
                obj.into_iter()
                    .map(|(k, v)| (TraverserValue::from(k), TraverserValue::from(v)))
                    .collect(),
            ),
            // A vertex or an edge value stays one value, see the `&ElementProperty` conversion.
            element @ (ElementProperty::Vertex(_) | ElementProperty::Edge(_)) => {
                TraverserValue::Computed(element.into_logical(), None)
            }
            ElementProperty::Map(map) => TraverserValue::Object(
                map.into_iter()
                    .map(|(k, v)| (TraverserValue::from(k), TraverserValue::from(v)))
                    .collect(),
            ),
        }
    }
}

impl<S: GraphStorage> From<&str> for TraverserValue<S> {
    fn from(value: &str) -> Self {
        TraverserValue::Computed(ElementProperty::String(CompactString::new(value)), None)
    }
}

impl<S: GraphStorage> From<String> for TraverserValue<S> {
    fn from(value: String) -> Self {
        TraverserValue::Computed(ElementProperty::String(CompactString::new(value)), None)
    }
}

impl<S: GraphStorage> From<CompactString> for TraverserValue<S> {
    fn from(value: CompactString) -> Self {
        TraverserValue::Computed(ElementProperty::String(value), None)
    }
}

impl<'a, S: GraphStorage> From<Cow<'a, str>> for TraverserValue<S> {
    fn from(value: Cow<'a, str>) -> Self {
        TraverserValue::Computed(ElementProperty::String(CompactString::new(value)), None)
    }
}

impl<S: GraphStorage> TraverserValue<S> {
    /// Returns the name of this stream value's kind, as it appears in diagnostics.
    ///
    /// This is the *stream* vocabulary — the kinds a traverser can carry, several of which are
    /// not property types at all. The vocabulary for property **types** is
    /// [`ValueType`](crate::graph::ValueType); use [`type_of`](crate::graph::type_of) for a value's type.
    /// Both sets of names are a stable contract (design decision D14).
    pub fn type_name(&self) -> &'static str {
        match self {
            TraverserValue::Computed(_, _) => "computed",
            TraverserValue::VertexProperty(_, _, _) => "vertexProperty",
            TraverserValue::Vertex(_) => "vertex",
            TraverserValue::Path(_) => "path",
            TraverserValue::Edge(_) => "edge",
            TraverserValue::EdgeProperty(_, _, _) => "edgeProperty",
            TraverserValue::Record { .. } => "record",
            TraverserValue::Object(_) => "object",
            TraverserValue::PathProperty(_) => "pathProperty",
            TraverserValue::Entry(_) => "entry",
            TraverserValue::Array(_) => "array",
            TraverserValue::Schema(_) => "schema",
            TraverserValue::Graph(_) => "graph",
        }
    }

    /// The kind a diagnostic reports for this value: the canonical [`ValueType`] name of a
    /// computed value (`int64`, `string`, `array<int64>`, ...), `object` for any map shape (a
    /// `Record` is an internal representation of one), and [`type_name`](Self::type_name)
    /// otherwise (`vertex`, `path`, `array`, `entry`, `pathProperty`, ...).
    ///
    /// [`ValueType`]: crate::graph::ValueType
    pub(crate) fn kind_name(&self) -> String {
        match self {
            TraverserValue::Computed(value, _) => crate::graph::schema::value_type_name(value),
            TraverserValue::Record { .. } => "object".to_string(),
            other => other.type_name().to_string(),
        }
    }

    /// Materializes this lazy value into an owned [`ElementProperty`], reading from `storage`.
    pub fn into_node_property(&self, storage: &S) -> TraverserResult<ElementProperty> {
        match self {
            TraverserValue::Computed(value, _) => Ok(value.to_logical()),

            TraverserValue::Vertex(node_index) => Self::materialize_vertex(storage, node_index),

            TraverserValue::Record { keys, values } => {
                let mut res: PropertyMap = PropertyMap::with_capacity(keys.len());
                for (key, val) in keys.iter().zip(values.iter()) {
                    // A token key is written as `id`/`label`/`OUT`/`IN`; a name key of the same
                    // text (a property named `id` or `label`) wins over it.
                    if key.is_token() && res.contains_key(key.as_str()) {
                        continue;
                    }
                    let value = match val {
                        TraverserValue::Array(array) => {
                            let mut array_vals = Vec::with_capacity(array.len());
                            for v in array {
                                array_vals.push(v.into_node_property(storage)?);
                            }
                            ElementProperty::Array(array_vals)
                        }
                        v => v.into_node_property(storage)?,
                    };
                    res.insert(key.to_compact(), value);
                }
                Ok(ElementProperty::Object(Box::new(res)))
            }

            TraverserValue::Object(obj) => {
                // Keys keep their type: an all-string key set builds an `Object` (the common path, no
                // extra allocation), any other key makes a `Map` (`ElementProperty::from_entries`).
                let mut res: PropertyMap = PropertyMap::with_capacity(obj.len());
                let mut typed: Option<ValueMap> = None;
                for (key_tv, val_tv) in obj.iter() {
                    let key = key_tv.into_node_property(storage)?;
                    let val = val_tv.into_node_property(storage)?;
                    if let Some(map) = typed.as_mut() {
                        map.insert(key, val);
                        continue;
                    }
                    match key {
                        ElementProperty::String(text) => {
                            res.insert(text, val);
                        }
                        other => {
                            let mut map = ValueMap::with_capacity(res.len() + 1);
                            for (k, v) in res.indexed_mut().drain(..) {
                                map.insert(ElementProperty::String(k), v);
                            }
                            map.insert(other, val);
                            typed = Some(map);
                        }
                    }
                }
                Ok(match typed {
                    Some(map) => map.into_property(),
                    None => ElementProperty::Object(Box::new(res)),
                })
            }

            TraverserValue::PathProperty(prop) => Ok(prop.1.to_logical()),

            TraverserValue::Entry(entry) => {
                let (key_tv, val_tv) = entry.as_ref();
                let key = key_tv.into_node_property(storage)?;
                let val = val_tv.into_node_property(storage)?;
                Ok(ElementProperty::from_entries([(key, val)]))
            }

            TraverserValue::Edge(edge_index) => Self::materialize_edge(storage, edge_index),

            TraverserValue::VertexProperty(property, node_index, kind) => {
                Self::materialize_vertex_property(storage, property, node_index, *kind)
            }

            TraverserValue::EdgeProperty(property, edge_index, kind) => {
                Self::materialize_edge_property(storage, property, edge_index, *kind)
            }

            TraverserValue::Path(path) => {
                let mut objects = Vec::with_capacity(path.len());
                for object in path.objects() {
                    objects.push(object.into_node_property(storage)?);
                }
                Ok(ElementProperty::Array(objects))
            }
            TraverserValue::Array(array) => {
                let mut values = Vec::with_capacity(array.len());
                for v in array {
                    values.push(v.into_node_property(storage)?);
                }
                Ok(ElementProperty::Array(values))
            }
            TraverserValue::Schema(schema) => Ok(schema.as_ref().into()),
            TraverserValue::Graph(graph) => crate::graph::subgraph::graph_to_property(graph),
        }
    }

    // Out of line: `into_node_property` recurses through nested values, so every arm that
    // does not recurse is kept out of its stack frame.
    #[inline(never)]
    fn materialize_vertex(
        storage: &S,
        node_index: &S::VertexId,
    ) -> TraverserResult<ElementProperty> {
        let id = crate::graph::storage::stored_vertex_id(storage, *node_index)?;
        let labels = crate::graph::storage::stored_vertex_labels(storage, *node_index)?.to_vec();
        let properties = storage
            .vertex_properties(*node_index)
            .map(logical_element)
            .unwrap_or_default();
        Ok(ElementProperty::Vertex(Box::new(VertexValue::new(
            id, labels, properties,
        ))))
    }

    // Out of line: `into_node_property` recurses through nested values, so every arm that
    // does not recurse is kept out of its stack frame.
    #[inline(never)]
    fn materialize_edge(storage: &S, edge_index: &S::EdgeId) -> TraverserResult<ElementProperty> {
        let id = crate::graph::storage::stored_edge_id(storage, *edge_index)?;
        let label = storage.edge_label(*edge_index).map(CompactString::new);
        let (out_vertex, in_vertex) = storage
            .edge_endpoints(*edge_index)
            .ok_or_else(|| Box::new(crate::error::GraphError::edge_removed().into()))?;
        let endpoint = |vertex: S::VertexId| -> TraverserResult<VertexRef> {
            Ok(VertexRef::new(
                crate::graph::storage::stored_vertex_id(storage, vertex)?,
                storage.vertex_label(vertex).map(CompactString::new),
            ))
        };
        let properties = storage
            .edge_properties(*edge_index)
            .map(logical_element)
            .unwrap_or_default();
        Ok(ElementProperty::Edge(Box::new(EdgeValue::new(
            id,
            label,
            endpoint(out_vertex)?,
            endpoint(in_vertex)?,
            properties,
        ))))
    }

    // Out of line: `into_node_property` recurses through nested values, so every arm that
    // does not recurse is kept out of its stack frame.
    #[inline(never)]
    fn materialize_vertex_property(
        storage: &S,
        property: &CompactString,
        node_index: &S::VertexId,
        kind: PropertyHandleKind,
    ) -> TraverserResult<ElementProperty> {
        if kind == PropertyHandleKind::Id {
            let id = crate::graph::storage::stored_vertex_id(storage, *node_index)?.to_string();
            Ok(ElementProperty::String(CompactString::new(id)))
        } else if kind == PropertyHandleKind::Label {
            Ok(
                crate::graph::storage::stored_vertex_label(storage, *node_index)?
                    .map(|l| ElementProperty::String(CompactString::new(l)))
                    .unwrap_or(ElementProperty::Null),
            )
        } else if kind == PropertyHandleKind::Labels {
            Ok(ElementProperty::Array(
                crate::graph::storage::stored_vertex_labels(storage, *node_index)?
                    .iter()
                    .map(|l| ElementProperty::String(l.clone()))
                    .collect(),
            ))
        } else {
            // A handle to a property of a vertex removed since: the value is gone.
            let Some(props) = storage.vertex_properties(*node_index) else {
                return Err(Box::new(crate::error::GraphError::vertex_removed().into()));
            };
            let prop_value = props.get(property.as_str());
            Ok(prop_value
                .map(|v| v.to_logical())
                .unwrap_or(ElementProperty::Null))
        }
    }

    // Out of line: `into_node_property` recurses through nested values, so every arm that
    // does not recurse is kept out of its stack frame.
    #[inline(never)]
    fn materialize_edge_property(
        storage: &S,
        property: &CompactString,
        edge_index: &S::EdgeId,
        kind: PropertyHandleKind,
    ) -> TraverserResult<ElementProperty> {
        if kind == PropertyHandleKind::Id {
            let id = crate::graph::storage::stored_edge_id(storage, *edge_index)?.to_string();
            Ok(ElementProperty::String(CompactString::new(id)))
        } else if kind == PropertyHandleKind::Label {
            Ok(
                crate::graph::storage::stored_edge_label(storage, *edge_index)?
                    .map(|l| ElementProperty::String(CompactString::new(l)))
                    .unwrap_or(ElementProperty::Null),
            )
        } else if kind == PropertyHandleKind::Labels {
            // An edge carries exactly one label; `labels()` still returns a list (D4), for
            // parity with the vertex side and a future Cypher relation pattern.
            Ok(ElementProperty::Array(
                crate::graph::storage::stored_edge_label(storage, *edge_index)?
                    .map(|l| vec![ElementProperty::String(CompactString::new(l))])
                    .unwrap_or_default(),
            ))
        } else {
            let props = storage.edge_properties(*edge_index);
            // An edge may store no property map at all, so only a removed edge (not a
            // missing map) is an error here.
            if props.is_none() && !storage.contains_edge(*edge_index) {
                return Err(Box::new(crate::error::GraphError::edge_removed().into()));
            }
            let prop_value = props.and_then(|map| map.get(property.as_str()));
            Ok(prop_value
                .map(|v| v.to_logical())
                .unwrap_or(ElementProperty::Null))
        }
    }

    /// This value as a schema reference, or `None` if it isn't one.
    pub fn as_schema(&self) -> Option<&GraphSchema> {
        match self {
            TraverserValue::Schema(schema) => Some(schema.as_ref()),
            _ => None,
        }
    }

    /// This value as an owned schema, or `None` if it isn't one.
    pub fn into_schema(self) -> Option<GraphSchema> {
        match self {
            TraverserValue::Schema(schema) => Some(*schema),
            _ => None,
        }
    }

    /// The cast error of `T.key`/`T.value` on a value that is not a property element or a map
    /// entry. A lazily read `values()` result names the type of the value it stands for.
    fn t_token_error(
        context: &TraverserContext<'_, S>,
        value: &TraverserValue<S>,
    ) -> Box<crate::error::TraverserError> {
        let actual = match value {
            TraverserValue::VertexProperty(_, _, kind)
            | TraverserValue::EdgeProperty(_, _, kind)
                if kind.is_value() =>
            {
                crate::traversal::step::local_scope::stored_value(context, value).map_or_else(
                    || "null".to_string(),
                    |stored| TraverserValue::<S>::Computed(stored.clone(), None).kind_name(),
                )
            }
            other => other.kind_name(),
        };
        Box::new(crate::error::TraverserError::cast_exception_str(
            crate::error::T_TOKEN_EXPECTED,
            actual,
        ))
    }

    /// Applies one `by()` modulator to a traverser, like TinkerPop's `TraversalUtil.produce()`:
    /// `Some` is the projected value (a `by(__...)` child's *first* result), `None` means the
    /// `by()` was unproductive (a missing property, a child yielding nothing). Each caller applies
    /// its own step's rule for an unproductive `by()` (filter the traverser, drop a key, skip a
    /// level, ...). Errors, including a failing child traversal, propagate.
    pub(crate) fn fetch_traverser_value_by<'g>(
        context: &mut TraverserContext<'_, S>,
        traverser: &Traverser<S>,
        traverser_value: &TraverserValue<S>,
        by_val: &By<'g, S>,
    ) -> TraverserResult<Option<TraverserValue<S>>> {
        Ok(match by_val {
            By::Identity => Some(traverser_value.clone()),
            // `T.value` (TinkerPop `T.value`) reads a property element: the stored value of a
            // `properties()` value, so `by(T.value)` compares and groups properties by value, not
            // by the handle. Like `T.key`, it also reads a jpath property and a map entry. Any
            // other value, a lazily read `values()` result included, is a cast error, as in
            // TinkerPop.
            By::Value => match traverser_value {
                TraverserValue::VertexProperty(_, _, PropertyHandleKind::Element)
                | TraverserValue::EdgeProperty(_, _, PropertyHandleKind::Element) => {
                    crate::traversal::step::local_scope::stored_value(context, traverser_value)
                        .map(|stored| TraverserValue::Computed(stored.clone(), None))
                }
                TraverserValue::PathProperty(property) => {
                    Some(TraverserValue::Computed(property.1.clone(), None))
                }
                TraverserValue::Entry(entry) => Some(entry.1.clone()),
                other => return Err(Self::t_token_error(context, other)),
            },
            // `T.key` reads the key of a property element, a jpath property or a map entry; any
            // other value, a `values()` result included, is a cast error, as for `T.value`.
            By::Key => match traverser_value {
                TraverserValue::VertexProperty(key, _, PropertyHandleKind::Element)
                | TraverserValue::EdgeProperty(key, _, PropertyHandleKind::Element) => Some(
                    TraverserValue::Computed(ElementProperty::String(key.clone()), None),
                ),
                TraverserValue::PathProperty(property) => Some(TraverserValue::Computed(
                    ElementProperty::String(property.0.clone()),
                    None,
                )),
                TraverserValue::Entry(entry) => Some(entry.0.clone()),
                other => return Err(Self::t_token_error(context, other)),
            },
            By::Label => {
                Self::reject_token_target(traverser_value, "T.label")?;
                let v = TraverserValue::fetch_label(context, traverser_value)?;
                v.map(|v| {
                    TraverserValue::Computed(
                        ElementProperty::String(CompactString::new(v.as_ref())),
                        None,
                    )
                })
            }
            By::Id => {
                Self::reject_token_target(traverser_value, "T.id")?;
                let v = TraverserValue::fetch_id(context, traverser_value)?;
                v.map(|v| {
                    TraverserValue::Computed(
                        ElementProperty::String(CompactString::new(v.as_ref())),
                        None,
                    )
                })
            }
            By::Property(prop) => {
                let v = TraverserValue::fetch_property(context, traverser_value, prop)?;
                // A nested `Object`/`Array` property must stay navigable by further steps
                // (`values()`, `select()`), not get sealed opaquely inside `Computed`; `.into()`
                // is the same recursive `ElementProperty` -> `TraverserValue` conversion used
                // throughout the value model (see `impl From<ElementProperty> for TraverserValue`).
                v.map(|v| v.into())
            }
            // TinkerPop's `TraversalUtil.produce()`: the child's first result, `None` (an
            // unproductive `by()`) when it yields nothing; a failing child fails the step. A
            // caller wanting every result writes `by(__....fold())`.
            By::Traversal(child) => child
                .first(&mut *context, traverser)?
                .map(|first| first.value),
            By::Order(_) => None,
            By::Count => None,
            By::JsonPath(path) => {
                TraverserValue::resolve_json_path(context, traverser_value, path.segments())
            }
            // A label list filters vertices; it projects nothing. Every step that resolves a
            // `by()` through here rejects it with `InvalidModulator` (via `ByModulatorCheck`)
            // before it gets here; this arm only keeps a future caller from silently ignoring it.
            By::Labels(labels) => {
                return Err(Box::new(crate::error::TraverserError::QueryExecution {
                    query: format!("by({})", crate::value::literal::rhai_string_array(labels)),
                    reason: "a vertex-label list is not a projection".to_string(),
                }));
            }
            By::Column(column) => TraverserValue::column_of(context, traverser_value, *column),
        })
    }

    /// Fails a `by(T.label)` / `by(T.id)` whose value cannot carry that token, instead of letting
    /// it read as "unproductive" and silently drop the traverser. TinkerPop's `TokenTraversal`
    /// (3.7.2 bytecode; 3.8.2 unchanged in the feature files) throws `IllegalStateException` for
    /// anything but an `Element`; here a vertex, an edge, a vertex property element and a
    /// materialized map (`elementMap()` output, which carries `id`/`label` entries) can carry a
    /// token. A vertex property element has no id in Graphersal (see `id()`).
    fn reject_token_target(value: &TraverserValue<S>, token: &'static str) -> TraverserResult<()> {
        let supported = match value {
            TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::Object(_)
            | TraverserValue::Record { .. } => true,
            TraverserValue::VertexProperty(_, _, PropertyHandleKind::Element)
            | TraverserValue::PathProperty(_) => {
                if token == "T.id" {
                    return Err(Box::new(crate::error::TraverserError::QueryExecution {
                        query: "id".to_string(),
                        reason:
                            crate::traversal::step::map_steps::step_id::PROPERTY_ELEMENT_ID_REASON
                                .to_string(),
                    }));
                }
                true
            }
            // Value handles hold scalars; the `label()`/`id()` output handles are resolved by
            // the fetch itself.
            TraverserValue::VertexProperty(_, _, kind)
            | TraverserValue::EdgeProperty(_, _, kind) => {
                matches!(kind, PropertyHandleKind::Id | PropertyHandleKind::Label)
            }
            TraverserValue::Computed(..)
            | TraverserValue::Path(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Array(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => false,
        };
        if supported {
            return Ok(());
        }
        Err(Box::new(crate::error::TraverserError::QueryExecution {
            query: format!("by({token})"),
            reason: format!(
                "{token} can only be read from a vertex, an edge or a vertex property, not from a {}",
                match value {
                    TraverserValue::VertexProperty(_, _, kind) if kind.is_value() => {
                        "property value"
                    }
                    TraverserValue::EdgeProperty(..) => "property value",
                    other => other.type_name(),
                }
            ),
        }))
    }

    /// `select(Column.keys)` of a path (TinkerPop `Path.labels()`): one list of step labels per
    /// position, in position order; an unlabelled position is an empty list. The labels of a
    /// position keep the order of the `as()` calls that gave them.
    pub(crate) fn path_labels(path: &crate::value::path::TraverserPath<S>) -> TraverserValue<S> {
        TraverserValue::Array(
            path.steps
                .iter()
                .map(|step| {
                    TraverserValue::Array(
                        step.label
                            .iter()
                            .flatten()
                            .map(|label| {
                                TraverserValue::Computed(
                                    ElementProperty::String(label.clone()),
                                    None,
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    /// `by(Column::Keys)` / `by(Column::Values)`: the key/value of a map entry, or the list of
    /// keys/values of a whole map (a lazy property handle holding a map included), the same
    /// projection `select(Column)` makes. `None` (an unproductive `by()`) for anything else.
    fn column_of(
        context: &TraverserContext<'_, S>,
        value: &TraverserValue<S>,
        column: crate::value::Column,
    ) -> Option<TraverserValue<S>> {
        use crate::value::Column;
        fn property_column<S: GraphStorage>(
            property: &ElementProperty,
            column: Column,
        ) -> Option<TraverserValue<S>> {
            match property {
                ElementProperty::Object(entries) => Some(TraverserValue::Array(match column {
                    Column::Keys => entries
                        .keys()
                        .map(|key| {
                            TraverserValue::Computed(ElementProperty::String(key.clone()), None)
                        })
                        .collect(),
                    Column::Values => entries
                        .values()
                        .cloned()
                        .map(TraverserValue::from)
                        .collect(),
                })),
                ElementProperty::Map(entries) => Some(TraverserValue::Array(match column {
                    Column::Keys => entries.keys().cloned().map(TraverserValue::from).collect(),
                    Column::Values => entries
                        .values()
                        .cloned()
                        .map(TraverserValue::from)
                        .collect(),
                })),
                ElementProperty::Null
                | ElementProperty::String(_)
                | ElementProperty::Compressed(_)
                | ElementProperty::Deduped(_)
                | ElementProperty::Number(_)
                | ElementProperty::Boolean(_)
                | ElementProperty::Uuid(_)
                | ElementProperty::Temporal(_)
                | ElementProperty::Vector(_)
                | ElementProperty::Vertex(_)
                | ElementProperty::Edge(_)
                | ElementProperty::Array(_) => None,
            }
        }
        match value {
            TraverserValue::Entry(entry) => Some(match column {
                Column::Keys => entry.0.clone(),
                Column::Values => entry.1.clone(),
            }),
            TraverserValue::Object(map) => Some(TraverserValue::Array(match column {
                Column::Keys => map.keys().cloned().collect(),
                Column::Values => map.values().cloned().collect(),
            })),
            TraverserValue::Record { keys, values } => Some(TraverserValue::Array(match column {
                Column::Keys => keys
                    .iter()
                    .map(|key| {
                        TraverserValue::Computed(ElementProperty::String(key.to_compact()), None)
                    })
                    .collect(),
                Column::Values => values.clone(),
            })),
            TraverserValue::Computed(property, _) => property_column(property, column),
            TraverserValue::PathProperty(prop) => property_column(&prop.1, column),
            TraverserValue::VertexProperty(_, _, _) | TraverserValue::EdgeProperty(_, _, _) => {
                crate::traversal::step::local_scope::stored_value(context, value)
                    .and_then(|stored| property_column(stored, column))
            }
            TraverserValue::Path(path) => match column {
                Column::Keys => Some(Self::path_labels(path)),
                Column::Values => Some(TraverserValue::Array(path.objects().cloned().collect())),
            },
            TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::Array(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => None,
        }
    }

    /// Resolves a JSONPath's segments against any stream value, dispatching on its shape the same
    /// way for both `By::JsonPath` (`.by(JsonPath(...))`) and the dedicated `.json_path()` step:
    /// a `Vertex`/`Edge` reads the first segment as a stored property key
    /// (`descend_properties`), a lazy `VertexProperty`/`EdgeProperty` reference is
    /// fetched from storage first and then walked, and an already-materialized value (`Object`,
    /// `Record`, `Computed(Object|Array, _)`, ...) is walked directly. `None` means "not found",
    /// the same silent-empty outcome as a missing `values(key)`, never an error.
    pub(crate) fn resolve_json_path(
        context: &TraverserContext<'_, S>,
        traverser_value: &TraverserValue<S>,
        segments: &[crate::value::json_path::JsonPathSegment],
    ) -> Option<TraverserValue<S>> {
        use crate::value::json_path::{
            JsonPathSegment, descend_properties, descend_property, walk_json_path,
        };
        // Descends by reference into stored data and clones only the value the path ends on,
        // never the whole top-level property.
        match traverser_value {
            TraverserValue::Vertex(node_index) => {
                let props = context.storage.vertex_properties(*node_index)?;
                descend_properties(props, segments).map(|leaf| leaf.clone().into())
            }
            TraverserValue::Edge(edge_index) => {
                let props = context.storage.edge_properties(*edge_index)?;
                descend_properties(props, segments).map(|leaf| leaf.clone().into())
            }
            TraverserValue::VertexProperty(prop_name, node_index, _) => {
                let root = context
                    .storage
                    .vertex_properties(*node_index)?
                    .get(prop_name.as_str())?;
                descend_property(root, segments).map(|leaf| leaf.clone().into())
            }
            TraverserValue::EdgeProperty(prop_name, edge_index, _) => {
                let root = context
                    .storage
                    .edge_properties(*edge_index)?
                    .get(prop_name.as_str())?;
                descend_property(root, segments).map(|leaf| leaf.clone().into())
            }
            TraverserValue::Computed(root, _) => {
                descend_property(root, segments).map(|leaf| leaf.clone().into())
            }
            // A materialized map: take the first step by reference, clone only that child.
            TraverserValue::Object(obj) => {
                let (JsonPathSegment::Key(first), rest) = segments.split_first()? else {
                    return None;
                };
                let lookup = TraverserValue::Computed(ElementProperty::String(first.clone()), None);
                walk_json_path(obj.get(&lookup)?.clone(), rest)
            }
            TraverserValue::Record { keys, values } => {
                let (JsonPathSegment::Key(first), rest) = segments.split_first()? else {
                    return None;
                };
                let idx = keys.iter().position(|k| k == first.as_str())?;
                walk_json_path(values[idx].clone(), rest)
            }
            TraverserValue::Array(arr) => {
                let (JsonPathSegment::Index(index), rest) = segments.split_first()? else {
                    return None;
                };
                let idx = crate::value::json_path::resolve_index(*index, arr.len())?;
                walk_json_path(arr.get(idx)?.clone(), rest)
            }
            _ => None,
        }
    }

    pub(crate) fn fetch_property(
        context: &TraverserContext<'_, S>,
        traverser_value: &TraverserValue<S>,
        prop: &str,
    ) -> TraverserResult<Option<ElementProperty>> {
        match traverser_value {
            TraverserValue::Computed(node_property, _) => Ok(match node_property {
                ElementProperty::Object(obj) => obj.get(prop).map(ElementProperty::to_logical),
                ElementProperty::Vertex(vertex) => vertex
                    .properties()
                    .get(prop)
                    .map(ElementProperty::to_logical),
                ElementProperty::Edge(edge) => {
                    edge.properties().get(prop).map(ElementProperty::to_logical)
                }
                ElementProperty::Map(map) => map.get_str(prop).map(ElementProperty::to_logical),
                ElementProperty::Null => None,
                ElementProperty::String(_)
                | ElementProperty::Compressed(_)
                | ElementProperty::Deduped(_) => None,
                ElementProperty::Array(_) => None,
                ElementProperty::Number(_) => None,
                ElementProperty::Boolean(_) => None,
                ElementProperty::Uuid(_)
                | ElementProperty::Temporal(_)
                | ElementProperty::Vector(_) => None,
            }),
            // An `id()`/`label()`/`labels()` handle stands for a string or a list: no named
            // property.
            TraverserValue::VertexProperty(_, _, kind)
            | TraverserValue::EdgeProperty(_, _, kind)
                if !kind.reads_property() =>
            {
                Ok(None)
            }
            TraverserValue::VertexProperty(vertex_property, vertex_index, _) => {
                let v = context.storage.vertex_properties(*vertex_index);
                let v = v.and_then(|v| v.get(vertex_property));
                match v {
                    Some(v) => Ok(match v {
                        ElementProperty::Object(obj) => {
                            obj.get(prop).map(ElementProperty::to_logical)
                        }
                        ElementProperty::Vertex(vertex) => vertex
                            .properties()
                            .get(prop)
                            .map(ElementProperty::to_logical),
                        ElementProperty::Edge(edge) => {
                            edge.properties().get(prop).map(ElementProperty::to_logical)
                        }
                        ElementProperty::Map(map) => {
                            map.get_str(prop).map(ElementProperty::to_logical)
                        }
                        ElementProperty::Null => None,
                        ElementProperty::String(_)
                        | ElementProperty::Compressed(_)
                        | ElementProperty::Deduped(_) => None,
                        ElementProperty::Array(_) => None,
                        ElementProperty::Number(_) => None,
                        ElementProperty::Boolean(_) => None,
                        ElementProperty::Uuid(_)
                        | ElementProperty::Temporal(_)
                        | ElementProperty::Vector(_) => None,
                    }),
                    None => Ok(None),
                }
            }
            TraverserValue::Vertex(vertex_index) => {
                let v = context.storage.vertex_properties(*vertex_index);
                Ok(v.and_then(|v| v.get(prop)).map(ElementProperty::to_logical))
            }
            TraverserValue::Edge(edge_index) => {
                let e = context.storage.edge_properties(*edge_index);
                Ok(e.and_then(|e| e.get(prop)).map(ElementProperty::to_logical))
            }
            TraverserValue::Record { keys, values } => {
                if let Some(idx) = keys.iter().position(|k| k == prop) {
                    values[idx].into_node_property(context.storage).map(Some)
                } else {
                    Ok(None)
                }
            }
            TraverserValue::EdgeProperty(edge_property, edge_index, _) => {
                let e = context.storage.edge_properties(*edge_index);
                let e = e.and_then(|e| e.get(edge_property));
                match e {
                    Some(v) => Ok(match v {
                        ElementProperty::Object(obj) => {
                            obj.get(prop).map(ElementProperty::to_logical)
                        }
                        ElementProperty::Vertex(vertex) => vertex
                            .properties()
                            .get(prop)
                            .map(ElementProperty::to_logical),
                        ElementProperty::Edge(edge) => {
                            edge.properties().get(prop).map(ElementProperty::to_logical)
                        }
                        ElementProperty::Map(map) => {
                            map.get_str(prop).map(ElementProperty::to_logical)
                        }
                        ElementProperty::Null => None,
                        ElementProperty::String(_)
                        | ElementProperty::Compressed(_)
                        | ElementProperty::Deduped(_) => None,
                        ElementProperty::Array(_) => None,
                        ElementProperty::Number(_) => None,
                        ElementProperty::Boolean(_) => None,
                        ElementProperty::Uuid(_)
                        | ElementProperty::Temporal(_)
                        | ElementProperty::Vector(_) => None,
                    }),
                    None => Ok(None),
                }
            }
            TraverserValue::Object(obj) => {
                let key = TraverserValue::Computed(
                    ElementProperty::String(CompactString::new(prop)),
                    None,
                );
                match obj.get(&key) {
                    Some(v) => v.into_node_property(context.storage).map(Some),
                    None => Ok(None),
                }
            }
            // A path or a list has no named property to fetch — not a "shouldn't happen" state,
            // just a structural mismatch. `fetch_id`/`fetch_label` already treat both as `None`
            // for the same reason; the caller (e.g. `select().by(prop)`) turns a `None` into a
            // normal `PropertyNotFound` diagnostic instead of the panic this used to be.
            TraverserValue::Path(_) => Ok(None),
            TraverserValue::PathProperty(_) | TraverserValue::Entry(_) => Ok(None),
            TraverserValue::Array(_) => Ok(None),
            TraverserValue::Schema(_) | TraverserValue::Graph(_) => Ok(None),
        }
    }

    pub(crate) fn fetch_id<'a>(
        context: &'a TraverserContext<'_, S>,
        traverser_value: &'a TraverserValue<S>,
    ) -> TraverserResult<Option<Cow<'a, str>>> {
        Ok(match traverser_value {
            TraverserValue::Vertex(vertex_index) => {
                let id = Cow::Borrowed(crate::graph::storage::stored_vertex_id(
                    &*context.storage,
                    *vertex_index,
                )?);
                Some(id)
            }
            TraverserValue::Edge(edge_index) => {
                let id = Cow::Borrowed(crate::graph::storage::stored_edge_id(
                    &*context.storage,
                    *edge_index,
                )?);
                Some(id)
            }
            TraverserValue::VertexProperty(_, vertex_index, kind) => {
                if *kind == PropertyHandleKind::Id {
                    let id = Cow::Borrowed(crate::graph::storage::stored_vertex_id(
                        &*context.storage,
                        *vertex_index,
                    )?);
                    Some(id)
                } else {
                    None
                }
            }
            TraverserValue::EdgeProperty(_, edge_index, kind) => {
                if *kind == PropertyHandleKind::Id {
                    let id = Cow::Borrowed(crate::graph::storage::stored_edge_id(
                        &*context.storage,
                        *edge_index,
                    )?);
                    Some(id)
                } else {
                    None
                }
            }

            TraverserValue::Object(obj) => {
                let id_key = TraverserValue::Computed(
                    ElementProperty::String(CompactString::new("id")),
                    None,
                );
                match obj.get(&id_key) {
                    Some(TraverserValue::Computed(ElementProperty::String(id), _)) => {
                        Some(Cow::Borrowed(id.as_str()))
                    }
                    _ => None,
                }
            }
            TraverserValue::Record { keys, values } => {
                if let Some(idx) = keys.iter().position(|k| *k == RecordKey::Id) {
                    match &values[idx] {
                        TraverserValue::Computed(ElementProperty::String(id), _) => {
                            Some(Cow::Borrowed(id.as_str()))
                        }
                        v => v
                            .into_node_property(context.storage)
                            .ok()
                            .and_then(|ep| match ep {
                                ElementProperty::String(s) => Some(Cow::Owned(s.into_string())),
                                _ => None,
                            }),
                    }
                } else {
                    None
                }
            }
            TraverserValue::Computed(_, _) => None,
            TraverserValue::Path(_) => None,
            TraverserValue::PathProperty(_) | TraverserValue::Entry(_) => None,
            TraverserValue::Array(_) => None,
            TraverserValue::Schema(_) | TraverserValue::Graph(_) => None,
        })
    }

    pub(crate) fn fetch_label<'a>(
        context: &'a TraverserContext<'_, S>,
        traverser_value: &'a TraverserValue<S>,
    ) -> TraverserResult<Option<Cow<'a, str>>> {
        Ok(match traverser_value {
            TraverserValue::Vertex(vertex_index) => {
                crate::graph::storage::stored_vertex_label(&*context.storage, *vertex_index)?
                    .map(Cow::Borrowed)
            }
            TraverserValue::Edge(edge_index) => {
                crate::graph::storage::stored_edge_label(&*context.storage, *edge_index)?
                    .map(Cow::Borrowed)
            }
            // A vertex property element (`properties()`) is labelled by its key, as in TinkerPop
            // (the same rule as the `label()` step and `hasLabel()`).
            TraverserValue::VertexProperty(key, _, PropertyHandleKind::Element) => {
                Some(Cow::Borrowed(key.as_str()))
            }
            TraverserValue::PathProperty(prop) => Some(Cow::Borrowed(prop.0.as_str())),
            TraverserValue::VertexProperty(_, vertex_index, kind) => {
                if *kind == PropertyHandleKind::Label {
                    crate::graph::storage::stored_vertex_label(&*context.storage, *vertex_index)?
                        .map(Cow::Borrowed)
                } else {
                    None
                }
            }
            TraverserValue::EdgeProperty(_, edge_index, kind) => {
                if *kind == PropertyHandleKind::Label {
                    crate::graph::storage::stored_edge_label(&*context.storage, *edge_index)?
                        .map(Cow::Borrowed)
                } else {
                    None
                }
            }
            TraverserValue::Object(obj) => {
                let key = TraverserValue::Computed(
                    ElementProperty::String(CompactString::new("label")),
                    None,
                );
                match obj.get(&key) {
                    Some(TraverserValue::Computed(ElementProperty::String(label), _)) => {
                        Some(Cow::Borrowed(label.as_str()))
                    }
                    Some(v) => v
                        .into_node_property(context.storage)
                        .ok()
                        .and_then(|ep| match ep {
                            ElementProperty::String(s) => Some(Cow::Owned(s.into_string())),
                            _ => None,
                        }),
                    None => None,
                }
            }
            TraverserValue::Record { keys, values } => {
                if let Some(idx) = keys.iter().position(|k| *k == RecordKey::Label) {
                    match &values[idx] {
                        TraverserValue::Computed(ElementProperty::String(label), _) => {
                            Some(Cow::Borrowed(label.as_str()))
                        }
                        v => v
                            .into_node_property(context.storage)
                            .ok()
                            .and_then(|ep| match ep {
                                ElementProperty::String(s) => Some(Cow::Owned(s.into_string())),
                                _ => None,
                            }),
                    }
                } else {
                    None
                }
            }
            TraverserValue::Computed(_, _) => None,
            TraverserValue::Path(_) => None,
            TraverserValue::Entry(_) => None,
            TraverserValue::Array(_) => None,
            TraverserValue::Schema(_) | TraverserValue::Graph(_) => None,
        })
    }
}

/// A step argument that is absent, one string, or a list of strings.
///
/// Only the explicit `None` (`None::<()>`, [`ArgStrings::None`]) is "no argument" (`out(None)` =
/// every edge label, `has_label(None)` = every labelled element). Converting a VALUE never yields
/// it: an [`ElementProperty`] that names nothing (null, a map, an object without a string `id`)
/// becomes an empty list, which matches nothing (`has_label(ElementProperty::Null)` keeps nothing,
/// like the DSL's `has_label(null)` and TinkerPop's `hasLabel(null)`).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ArgStrings {
    /// No argument given. Produced only by `None::<()>` (or by naming the variant), never by
    /// converting a value.
    None,
    /// One string.
    String(String),
    /// A list of strings.
    List(Vec<String>),
}

impl From<String> for ArgStrings {
    fn from(value: String) -> Self {
        ArgStrings::String(value)
    }
}

impl From<&str> for ArgStrings {
    fn from(value: &str) -> Self {
        ArgStrings::String(value.to_string())
    }
}

impl From<Vec<&str>> for ArgStrings {
    fn from(value: Vec<&str>) -> Self {
        ArgStrings::List(value.iter().map(|x| x.to_string()).collect())
    }
}

impl From<Vec<String>> for ArgStrings {
    fn from(value: Vec<String>) -> Self {
        ArgStrings::List(value)
    }
}

impl From<&Vec<String>> for ArgStrings {
    fn from(value: &Vec<String>) -> Self {
        ArgStrings::List(value.clone())
    }
}

impl From<CompactString> for ArgStrings {
    fn from(value: CompactString) -> Self {
        ArgStrings::String(value.into_string())
    }
}

impl From<&CompactString> for ArgStrings {
    fn from(value: &CompactString) -> Self {
        ArgStrings::String(value.to_string())
    }
}

impl From<Vec<CompactString>> for ArgStrings {
    fn from(value: Vec<CompactString>) -> Self {
        ArgStrings::List(value.into_iter().map(|x| x.into_string()).collect())
    }
}

impl From<&Vec<CompactString>> for ArgStrings {
    fn from(value: &Vec<CompactString>) -> Self {
        ArgStrings::List(value.iter().map(|x| x.to_string()).collect())
    }
}

/// `None::<()>` is the explicit "no argument" ([`ArgStrings::None`]).
impl From<Option<()>> for ArgStrings {
    fn from(_value: Option<()>) -> Self {
        ArgStrings::None
    }
}

/// The text of one item of a string-argument list (labels, keys, ids).
///
/// A string is its own text and any other value its physical text (a number `42`, a `uuid`
/// canonically); a value that can never name a label or key (null, a container, an element) is
/// its JSON text, which simply matches nothing. Never the value's `Debug` rendering.
fn arg_text(value: &ElementProperty) -> String {
    match value {
        ElementProperty::String(text) => text.to_string(),
        ElementProperty::Compressed(text) => text.text().into_string(),
        ElementProperty::Deduped(text) => text.as_str().to_string(),
        // A uuid names itself by its canonical text, anything else by its compact JSON text. Only a
        // non-finite float nested in a container makes the conversion fail; the text must merely be
        // deterministic, since no label or key can equal it.
        ElementProperty::Uuid(_) => physical::to_display_text(value).into_string(),
        other => physical::to_json_text(other).unwrap_or_else(|err| err.to_string()),
    }
}

/// A value as a step argument: a string (or compressed text) is one name, an array a list of names
/// (each item's text: a string itself, anything else its physical or JSON text), a number, boolean
/// or uuid its text, an element or object its string `id`. A value that names nothing (null, a
/// map, an object without a string `id`) is an EMPTY LIST, which matches nothing; never
/// [`ArgStrings::None`] ("no argument"). So `has_label(ElementProperty::Null)` keeps nothing while
/// `has_label(None::<()>)` keeps the labelled elements; likewise `v(Null)`, `out(Null)` and
/// `values(Null)` yield nothing, and a step that needs a name (`add_v`, `add_e`, `tree`, `cap`)
/// gets an empty list instead of its no-argument form.
impl From<ElementProperty> for ArgStrings {
    fn from(value: ElementProperty) -> Self {
        match value {
            ElementProperty::String(value) => ArgStrings::String(value.into_string()),
            ElementProperty::Compressed(text) => ArgStrings::String(text.text().into_string()),
            ElementProperty::Deduped(text) => ArgStrings::String(text.as_str().to_string()),
            ElementProperty::Object(obj) => {
                let id = obj.get("id");
                match id {
                    Some(ElementProperty::String(value)) => ArgStrings::String(value.to_string()),
                    _ => ArgStrings::List(Vec::new()),
                }
            }
            ElementProperty::Vertex(vertex) => ArgStrings::String(vertex.id().to_string()),
            ElementProperty::Edge(edge) => ArgStrings::String(edge.id().to_string()),
            ElementProperty::Null | ElementProperty::Map(_) => ArgStrings::List(Vec::new()),
            ElementProperty::Array(array) => ArgStrings::List(array.iter().map(arg_text).collect()),
            ElementProperty::Number(n) => ArgStrings::String(n.n.to_string()),
            ElementProperty::Boolean(b) => ArgStrings::String(b.to_string()),
            ElementProperty::Uuid(u) => ArgStrings::String(uuid::format_canonical(u).into_string()),
            ElementProperty::Temporal(t) => ArgStrings::String(t.to_string()),
            // A vector names nothing.
            ElementProperty::Vector(_) => ArgStrings::List(Vec::new()),
        }
    }
}

/// The same as the owned conversion `From<ElementProperty>`: a value that names nothing is an
/// empty list, never [`ArgStrings::None`].
impl From<&ElementProperty> for ArgStrings {
    fn from(value: &ElementProperty) -> Self {
        match value {
            ElementProperty::String(value) => ArgStrings::String(value.to_string()),
            ElementProperty::Compressed(text) => ArgStrings::String(text.text().into_string()),
            ElementProperty::Deduped(text) => ArgStrings::String(text.as_str().to_string()),
            ElementProperty::Object(obj) => {
                let id = obj.get("id");
                match id {
                    Some(ElementProperty::String(value)) => ArgStrings::String(value.to_string()),
                    _ => ArgStrings::List(Vec::new()),
                }
            }
            ElementProperty::Vertex(vertex) => ArgStrings::String(vertex.id().to_string()),
            ElementProperty::Edge(edge) => ArgStrings::String(edge.id().to_string()),
            ElementProperty::Null | ElementProperty::Map(_) => ArgStrings::List(Vec::new()),
            ElementProperty::Array(array) => ArgStrings::List(array.iter().map(arg_text).collect()),
            ElementProperty::Number(n) => ArgStrings::String(n.n.to_string()),
            ElementProperty::Boolean(b) => ArgStrings::String(b.to_string()),
            ElementProperty::Uuid(u) => {
                ArgStrings::String(uuid::format_canonical(*u).into_string())
            }
            ElementProperty::Temporal(t) => ArgStrings::String(t.to_string()),
            // A vector names nothing.
            ElementProperty::Vector(_) => ArgStrings::List(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::TraversalGraph;
    use fnv::FnvHasher;
    use std::cmp::Ordering;
    use std::hash::{Hash, Hasher};

    fn calculate_hash<T: Hash>(t: &T) -> u64 {
        let mut s = FnvHasher::default();
        t.hash(&mut s);
        s.finish()
    }

    #[test]
    fn test_traverser_value_hash_and_eq_without_panics() {
        // Path variant
        let path: TraverserPath<TraversalGraph> = TraverserPath { steps: vec![] };
        let val_path = TraverserValue::Path(path.clone());
        assert_eq!(val_path, val_path);
        let _ = calculate_hash(&val_path);

        // Object variant
        let mut obj: StreamMap<TraversalGraph> = StreamMap::default();
        let k = TraverserValue::Computed(ElementProperty::String("key".into()), None);
        let v = TraverserValue::Computed(ElementProperty::Number(Number::from_i64(42)), None);
        obj.insert(k, v);
        let val_obj = TraverserValue::Object(obj);
        assert_eq!(val_obj, val_obj);
        let _ = calculate_hash(&val_obj);

        // Array variant
        let arr: Vec<TraverserValue<TraversalGraph>> = vec![
            TraverserValue::Computed(ElementProperty::Boolean(true), None),
            TraverserValue::Computed(ElementProperty::Number(Number::from_f64(1.25)), None),
        ];
        let val_arr = TraverserValue::Array(arr);
        assert_eq!(val_arr, val_arr);
        let _ = calculate_hash(&val_arr);
    }

    #[test]
    fn test_compact_string_inline_and_conversions() {
        let short_str = "hello_world"; // 11 bytes <= 24 bytes inline capacity
        let compact = CompactString::new(short_str);
        assert!(!compact.is_heap_allocated());

        let ep_from_str: ElementProperty = short_str.into();
        assert_eq!(ep_from_str.as_str(), Some("hello_world"));
        assert_eq!(ep_from_str.as_string(), Some("hello_world"));

        let string_val = String::from("a_slightly_longer_string_for_testing");
        let ep_from_string: ElementProperty = string_val.clone().into();
        assert_eq!(
            ep_from_string.as_str(),
            Some("a_slightly_longer_string_for_testing")
        );
        assert_eq!(
            ep_from_string.as_string(),
            Some("a_slightly_longer_string_for_testing")
        );

        let cow_val: Cow<'_, str> = Cow::Borrowed("cow_string");
        let ep_from_cow: ElementProperty = cow_val.into();
        assert_eq!(ep_from_cow.as_str(), Some("cow_string"));

        let tv_from_str: TraverserValue<TraversalGraph> = short_str.into();
        match tv_from_str {
            TraverserValue::Computed(ElementProperty::String(cs), None) => {
                assert!(!cs.is_heap_allocated());
                assert_eq!(cs.as_str(), "hello_world");
            }
            _ => panic!("Expected Computed(ElementProperty::String)"),
        }
    }

    /// Boxing the heavy variants and adding the inline `Uuid(u128)` (design decision D6/D7)
    /// measurably shrinks the enum: 64 bytes before this step, 32 bytes after. `align_of` is 16
    /// because `u128` requires 16-byte alignment, so the 32-byte size is one alignment step, not
    /// two, above the smallest possible layout.
    #[test]
    fn element_property_is_boxed_down_to_32_bytes() {
        assert_eq!(std::mem::size_of::<ElementProperty>(), 32);
        assert_eq!(std::mem::align_of::<ElementProperty>(), 16);
    }

    fn sample_object() -> PropertyMap {
        let mut inner: PropertyMap = PropertyMap::default();
        inner.insert(
            CompactString::new("k"),
            ElementProperty::Number(Number::from_i64(1)),
        );
        let mut outer: PropertyMap = PropertyMap::default();
        outer.insert(
            CompactString::new("name"),
            ElementProperty::String("alice".into()),
        );
        outer.insert(
            CompactString::new("tags"),
            ElementProperty::Array(vec!["x".into(), "y".into()]),
        );
        outer.insert(
            CompactString::new("nested"),
            ElementProperty::Object(Box::new(inner)),
        );
        outer
    }

    #[test]
    fn object_converts_to_traverser_value_recursively_without_panic() {
        let ep = ElementProperty::Object(Box::new(sample_object()));
        let tv: TraverserValue<TraversalGraph> = (&ep).into();
        match &tv {
            TraverserValue::Object(obj) => assert_eq!(obj.len(), 3),
            _ => panic!("expected TraverserValue::Object"),
        }
        // Owned conversion path must agree with the by-reference one.
        let tv_owned: TraverserValue<TraversalGraph> = ep.into();
        assert_eq!(tv, tv_owned);
    }

    #[test]
    fn vertex_and_edge_element_properties_stay_element_values_on_the_stream() {
        let vertex =
            ElementProperty::Vertex(Box::new(VertexValue::new("1", Vec::new(), sample_object())));
        let tv: TraverserValue<TraversalGraph> = vertex.into();
        assert!(matches!(
            tv,
            TraverserValue::Computed(ElementProperty::Vertex(_), _)
        ));

        let edge = ElementProperty::Edge(Box::new(EdgeValue::new(
            "0",
            None,
            VertexRef::new("1", None),
            VertexRef::new("2", None),
            sample_object(),
        )));
        let tv: TraverserValue<TraversalGraph> = edge.into();
        assert!(matches!(
            tv,
            TraverserValue::Computed(ElementProperty::Edge(_), _)
        ));
    }

    #[test]
    fn argument_lists_render_items_as_physical_text_never_debug() {
        let list: ArgStrings = ElementProperty::Array(vec![
            ElementProperty::String("person".into()),
            ElementProperty::Number(Number::from_i64(42)),
            ElementProperty::Null,
            ElementProperty::Object(Box::new(sample_object())),
            ElementProperty::Vertex(Box::new(VertexValue::new(
                "v1",
                vec!["person".into()],
                sample_object(),
            ))),
        ])
        .into();
        let ArgStrings::List(items) = list else {
            panic!("an array converts to an argument list");
        };
        assert_eq!(items[0], "person");
        assert_eq!(items[1], "42");
        assert_eq!(items[2], "null");
        let json = r#"{"name":"alice","tags":["x","y"],"nested":{"k":1}}"#;
        assert_eq!(items[3], json);
        assert_eq!(
            items[4],
            format!(r#"{{"id":"v1","label":"person","properties":{json}}}"#)
        );
    }

    #[test]
    fn schema_mode_is_spelled_as_in_the_schema_file() {
        let schema = GraphSchema::new().with_mode(crate::graph::schema::SchemaMode::Open);
        let ElementProperty::Object(map) = schema.to_value() else {
            panic!("a schema renders as an object");
        };
        assert_eq!(
            map.get("mode"),
            Some(&ElementProperty::String("open".into()))
        );
    }

    #[test]
    fn scalar_arguments_convert_without_panic() {
        let number_arg: ArgStrings = ElementProperty::Number(Number::from_i64(42)).into();
        assert!(matches!(number_arg, ArgStrings::String(s) if s == "42"));
        let bool_arg: ArgStrings = (&ElementProperty::Boolean(false)).into();
        assert!(matches!(bool_arg, ArgStrings::String(s) if s == "false"));
    }

    /// A value that names nothing converts to an empty list (matches nothing), never to
    /// `ArgStrings::None` ("no argument"); only `None::<()>` is "no argument".
    #[test]
    fn values_naming_nothing_are_an_empty_list_never_no_argument() {
        let empty = |arg: ArgStrings| matches!(arg, ArgStrings::List(items) if items.is_empty());
        let map = ElementProperty::Map(Default::default());
        let no_id = ElementProperty::Object(Box::new(
            [("id", ElementProperty::Number(Number::from_i64(1)))]
                .into_iter()
                .collect(),
        ));
        for value in [
            ElementProperty::Null,
            map,
            no_id,
            ElementProperty::Object(Default::default()),
        ] {
            assert!(empty((&value).into()), "{value:?} by reference");
            assert!(empty(value.clone().into()), "{value:?} owned");
        }
        let with_id = ElementProperty::Object(Box::new(
            [("id", ElementProperty::String("v1".into()))]
                .into_iter()
                .collect(),
        ));
        assert!(matches!(ArgStrings::from(with_id), ArgStrings::String(s) if s == "v1"));
        assert!(matches!(ArgStrings::from(None::<()>), ArgStrings::None));
    }

    /// `fetch_property`'s `EdgeProperty`/`Object` arms and `fetch_label`'s `Object` arm (D1) are
    /// never reached by the public DSL today (there is no `properties()` step yet), so this
    /// exercises the `pub(crate)` functions directly rather than through a query.
    #[test]
    fn fetch_property_and_label_handle_edge_property_and_object_arms_without_panic() {
        use crate::traversal::context::{TraversalStrategy, TraverserContext};

        let mut storage = TraversalGraph::tinkerpop_modern();
        let edge_index = storage.get_edge_by_id("0").unwrap().0;
        let context =
            TraverserContext::new(&mut storage, TraversalStrategy::none(), Default::default());

        // EdgeProperty arm: "weight" holds a plain Number, so a nested lookup on it finds
        // nothing, but must return `Ok(None)`, not panic.
        let edge_prop = TraverserValue::EdgeProperty(
            CompactString::new("weight"),
            edge_index,
            PropertyHandleKind::Value,
        );
        assert_eq!(
            TraverserValue::fetch_property(&context, &edge_prop, "anything").unwrap(),
            None
        );

        // Object arm: key present -> Some(value); key absent -> None. Neither panics.
        let mut obj: StreamMap<TraversalGraph> = StreamMap::default();
        obj.insert(
            TraverserValue::Computed(ElementProperty::String("name".into()), None),
            TraverserValue::Computed(ElementProperty::String("alice".into()), None),
        );
        let object_value = TraverserValue::Object(obj);
        assert_eq!(
            TraverserValue::fetch_property(&context, &object_value, "name").unwrap(),
            Some(ElementProperty::String("alice".into()))
        );
        assert_eq!(
            TraverserValue::fetch_property(&context, &object_value, "missing").unwrap(),
            None
        );

        // fetch_label's Object arm mirrors the same lookup for a "label" key.
        assert_eq!(
            TraverserValue::fetch_label(&context, &object_value).unwrap(),
            None
        );
        let mut labeled_obj: StreamMap<TraversalGraph> = StreamMap::default();
        labeled_obj.insert(
            TraverserValue::Computed(ElementProperty::String("label".into()), None),
            TraverserValue::Computed(ElementProperty::String("person".into()), None),
        );
        let labeled_value = TraverserValue::Object(labeled_obj);
        let label = TraverserValue::fetch_label(&context, &labeled_value).unwrap();
        assert_eq!(label.as_deref(), Some("person"));
    }

    /// Regression guard for boxing `TraverserValue::Schema` (2026-09-24 spec): before that change,
    /// the unboxed `GraphSchema` variant (184 bytes) dominated the enum's size and made every
    /// `Traverser` 208 bytes regardless of which variant it actually carried. After boxing it, the
    /// largest remaining variant is `StreamMap<S>` (56 bytes), and `size_of::<Traverser<_>>()`
    /// measures 80 bytes (72 before the 8-byte bulk field was added). This asserts a loose upper bound, not the exact figure (struct layout is
    /// not a stable contract to pin), so an unrelated future field addition to some other variant
    /// doesn't spuriously fail this test — it exists only to catch a future accidental
    /// re-introduction of an oversized unboxed variant the way `Schema` was.
    #[test]
    fn traverser_stays_small_after_boxing_schema() {
        assert!(
            std::mem::size_of::<crate::traversal::traverser::Traverser<TraversalGraph>>() <= 96,
            "Traverser<TraversalGraph> grew past its expected small-handle size; check for a new \
             unboxed large variant in TraverserValue"
        );
    }

    // --- compare_element_property (order() Part 1: total-order comparison infrastructure) ---

    #[test]
    fn compare_element_property_numeric_cross_sub_type_equality_and_ordering() {
        let int_one = ElementProperty::Number(Number::from_i64(1));
        let float_one = ElementProperty::Number(Number::from_f64(1.0));
        let int_two = ElementProperty::Number(Number::from_i64(2));
        let float_half = ElementProperty::Number(Number::from_f64(0.5));

        // An int and a float of equal value are equal, across sub-types.
        assert_eq!(
            compare_element_property(&int_one, &float_one),
            Ordering::Equal
        );
        assert_eq!(
            compare_element_property(&float_one, &int_one),
            Ordering::Equal
        );

        // Ordering holds across sub-types too, not just within one.
        assert_eq!(compare_element_property(&int_one, &int_two), Ordering::Less);
        assert_eq!(
            compare_element_property(&int_two, &int_one),
            Ordering::Greater
        );
        assert_eq!(
            compare_element_property(&float_half, &int_one),
            Ordering::Less
        );
        assert_eq!(
            compare_element_property(&int_one, &float_half),
            Ordering::Greater
        );
    }

    #[test]
    fn compare_element_property_string_lexicographic_ordering() {
        let apple = ElementProperty::String(CompactString::new("apple"));
        let banana = ElementProperty::String(CompactString::new("banana"));
        let apple2 = ElementProperty::String(CompactString::new("apple"));

        assert_eq!(compare_element_property(&apple, &banana), Ordering::Less);
        assert_eq!(compare_element_property(&banana, &apple), Ordering::Greater);
        assert_eq!(compare_element_property(&apple, &apple2), Ordering::Equal);
    }

    #[test]
    fn compare_element_property_boolean_false_before_true() {
        let f = ElementProperty::Boolean(false);
        let t = ElementProperty::Boolean(true);

        assert_eq!(compare_element_property(&f, &t), Ordering::Less);
        assert_eq!(compare_element_property(&t, &f), Ordering::Greater);
        assert_eq!(compare_element_property(&f, &f), Ordering::Equal);
    }

    /// The load-bearing test for the whole `order()` series (per the spec): comparing genuinely
    /// incomparable types (a `Number` and a `String`) must never panic, and must produce a
    /// defined, stable result — the fixed type-rank order documented on `type_rank`, not an
    /// arbitrary or content-sniffed decision (rule 7: a string never gets inspected for whether it
    /// "looks like" a number).
    #[test]
    fn compare_element_property_mixed_types_do_not_panic_and_use_defined_type_rank() {
        let number = ElementProperty::Number(Number::from_i64(42));
        let string = ElementProperty::String(CompactString::new("42"));
        let boolean = ElementProperty::Boolean(true);
        let null = ElementProperty::Null;

        // Defined rank: Null < Boolean < Number < String < Uuid < Array < Object < Vertex < Edge.
        assert_eq!(compare_element_property(&null, &boolean), Ordering::Less);
        assert_eq!(compare_element_property(&boolean, &number), Ordering::Less);
        assert_eq!(compare_element_property(&number, &string), Ordering::Less);
        assert_eq!(
            compare_element_property(&string, &number),
            Ordering::Greater
        );

        // Stable and symmetric: comparing the same mixed pair repeatedly gives the same answer,
        // and swapping the operands flips the result rather than panicking or erroring.
        for _ in 0..3 {
            assert_eq!(compare_element_property(&number, &string), Ordering::Less);
        }

        // A full heterogeneous slice sorts without panicking.
        let mut values = vec![
            ElementProperty::String(CompactString::new("z")),
            ElementProperty::Number(Number::from_f64(3.5)),
            ElementProperty::Boolean(false),
            ElementProperty::Null,
            ElementProperty::Uuid(7),
        ];
        values.sort_by(compare_element_property);
        assert_eq!(
            values,
            vec![
                ElementProperty::Null,
                ElementProperty::Boolean(false),
                ElementProperty::Number(Number::from_f64(3.5)),
                ElementProperty::String(CompactString::new("z")),
                ElementProperty::Uuid(7),
            ]
        );
    }

    #[test]
    fn compare_element_property_array_element_wise_then_by_length() {
        let short = ElementProperty::Array(vec![ElementProperty::Number(Number::from_i64(1))]);
        let long = ElementProperty::Array(vec![
            ElementProperty::Number(Number::from_i64(1)),
            ElementProperty::Number(Number::from_i64(2)),
        ]);
        let bigger_first =
            ElementProperty::Array(vec![ElementProperty::Number(Number::from_i64(9))]);

        // Common prefix agrees, so the shorter array sorts first.
        assert_eq!(compare_element_property(&short, &long), Ordering::Less);
        // First element decides before length is even considered.
        assert_eq!(
            compare_element_property(&short, &bigger_first),
            Ordering::Less
        );
        assert_eq!(
            compare_element_property(&short, &short.clone()),
            Ordering::Equal
        );
    }

    /// Elements order by id (TinkerPop), not by their first differing map entry; a vertex ranks
    /// before an edge.
    #[test]
    fn compare_element_property_orders_elements_by_id_and_vertices_before_edges() {
        fn element(entries: &[(&str, &str)]) -> PropertyMap {
            let mut element = PropertyMap::default();
            for (key, value) in entries {
                element.insert(
                    CompactString::new(key),
                    ElementProperty::String(CompactString::new(value)),
                );
            }
            element
        }
        fn edge(id: &str, out_v: &str, in_v: &str) -> ElementProperty {
            ElementProperty::Edge(Box::new(EdgeValue::new(
                id,
                Some("knows".into()),
                VertexRef::new(out_v, None),
                VertexRef::new(in_v, None),
                PropertyMap::default(),
            )))
        }
        fn vertex(id: &str, entries: &[(&str, &str)]) -> ElementProperty {
            ElementProperty::Vertex(Box::new(VertexValue::new(id, Vec::new(), element(entries))))
        }
        // The in-vertex would order 11 before 10; the id decides.
        let edge_10 = edge("10", "4", "5");
        let edge_11 = edge("11", "4", "3");
        assert_eq!(compare_element_property(&edge_10, &edge_11), Ordering::Less);
        assert_eq!(
            compare_element_property(&edge_11, &edge_10),
            Ordering::Greater
        );
        assert_eq!(
            compare_element_property(&edge_10, &edge_10.clone()),
            Ordering::Equal
        );

        let vertex_9 = vertex("9", &[("name", "z")]);
        assert_eq!(
            compare_element_property(&vertex_9, &edge_10),
            Ordering::Less
        );
        assert_eq!(
            compare_element_property(&edge_10, &vertex_9),
            Ordering::Greater
        );

        // Equal ids fall back to the structural comparison of the properties.
        let same_id_a = vertex("1", &[("name", "a")]);
        let same_id_b = vertex("1", &[("name", "b")]);
        assert_eq!(
            compare_element_property(&same_id_a, &same_id_b),
            Ordering::Less
        );
    }
}
