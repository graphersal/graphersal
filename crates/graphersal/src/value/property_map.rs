//! [`PropertyMap`], a graph element's insertion-ordered property map.
//!
//! A newtype over an internal representation so that the map crate behind it (`indexmap` with an
//! FNV hasher) stays out of the public API: a major version of that crate cannot break a user.
//!
//! The representation is one of two (crate-internal [`Repr`]):
//!
//! - `Indexed`: an `IndexMap`, for every free-standing map (nested objects, catalog payloads,
//!   change sets, results, schema values) and every map a caller builds.
//! - `Shaped`: a shared, interned key list ([`Shape`], held through an `Arc`) plus the values in
//!   key order (a `Vec<ElementProperty>`), for the maps a `TraversalGraph` stores per element:
//!   elements with the same key sequence share one key list, so a property slot costs the value
//!   alone. The graph's shape table (`graph/shape.rs`) interns the shapes and moves a stored map
//!   from shape to shape on key-adding and key-removing writes.
//!
//! Both representations behave identically through the public API (order rules, order-independent
//! equality, `Debug` text, serde). A key-changing mutation of a `Shaped` map through the public API
//! (outside the graph's shape table) turns it into an `Indexed` map first; a value-only mutation
//! (an existing key, `get_mut`, `iter_mut`, `values_mut`) keeps it `Shaped`.
//!
//! Design and invariants: the book page "In-Memory Storage Internals"
//! (`book/src/for_developers/storage_internals.md`).

use std::fmt;
use std::hash::BuildHasherDefault;
use std::iter::FusedIterator;
use std::sync::Arc;

use compact_str::CompactString;
use fnv::FnvHasher;
use indexmap::{IndexMap, IndexSet};

use super::ElementProperty;

/// The `IndexMap` behind an `Indexed` [`PropertyMap`] (crate-internal).
pub(crate) type PropertyMapInner =
    IndexMap<CompactString, ElementProperty, BuildHasherDefault<FnvHasher>>;

/// The key count above which a [`Shape`] keeps a hash index of its keys; at or below it a key is
/// found by a linear scan of the key prints (cheaper for short lists than hashing the key).
pub(crate) const WIDE_SHAPE: usize = 16;

/// The longest key a [`KeyPrint`] identifies exactly (no byte comparison needed on a match).
const EXACT_PRINT: usize = 16;

/// A cheap fingerprint of a key: its length and its first and last eight bytes (zero-padded).
/// Two keys of at most [`EXACT_PRINT`] bytes are equal iff their prints are; longer keys with
/// equal prints are confirmed by comparing the bytes. Comparing prints avoids a `memcmp` call per
/// candidate key, which dominated the narrow-shape lookups.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyPrint {
    len: usize,
    head: u64,
    tail: u64,
}

impl KeyPrint {
    /// The print of `key`.
    #[inline]
    pub(crate) fn of(key: &str) -> Self {
        let bytes = key.as_bytes();
        let word4 = |chunk: Option<&[u8; 4]>| chunk.map_or(0, |c| u32::from_le_bytes(*c) as u64);
        let word2 = |chunk: Option<&[u8; 2]>| chunk.map_or(0, |c| u16::from_le_bytes(*c) as u64);
        // Overlapping head and tail words cover every byte of a key of up to 16 bytes, so with
        // the length the print is exact for those.
        let (head, tail) = match (bytes.first_chunk::<8>(), bytes.last_chunk::<8>()) {
            (Some(head), Some(tail)) => (u64::from_le_bytes(*head), u64::from_le_bytes(*tail)),
            _ if bytes.len() >= 4 => (
                word4(bytes.first_chunk::<4>()) | (word4(bytes.last_chunk::<4>()) << 32),
                0,
            ),
            _ if bytes.len() >= 2 => (
                word2(bytes.first_chunk::<2>()) | (word2(bytes.last_chunk::<2>()) << 16),
                0,
            ),
            _ => (bytes.first().map_or(0, |byte| *byte as u64), 0),
        };
        Self {
            len: bytes.len(),
            head,
            tail,
        }
    }

    /// Whether a key whose print equals this one is certainly `stored` (the print is exact for
    /// short keys; a longer key compares its bytes).
    #[inline]
    pub(crate) fn confirms(&self, stored: &CompactString, key: &str) -> bool {
        self.len <= EXACT_PRINT || stored.as_str() == key
    }
}

/// The hash of a key sequence, the interning key of a [`Shape`] (FNV over each key's bytes and a
/// terminator, the same as `str`'s `Hash`).
#[inline]
pub(crate) fn keys_hash<'a>(keys: impl Iterator<Item = &'a str>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = FnvHasher::default();
    for key in keys {
        key.hash(&mut hasher);
    }
    hasher.finish()
}

/// An ordered key list shared by every stored property map with exactly these keys in exactly
/// this order (crate-internal). Immutable once built; `owner` is the id of the shape table that
/// interned it (`graph/shape.rs`). No interior mutability, so the map and its iterators keep
/// their auto traits.
pub(crate) struct Shape {
    /// The keys in order (unique).
    pub(crate) keys: Box<[CompactString]>,
    /// One [`KeyPrint`] per key, in order: the linear lookup compares these.
    prints: Box<[KeyPrint]>,
    /// Key -> position for a wide shape (more than [`WIDE_SHAPE`] keys), else `None`.
    index: Option<IndexSet<CompactString, BuildHasherDefault<FnvHasher>>>,
    /// The id of the shape table that interned this shape.
    pub(crate) owner: u64,
    /// [`keys_hash`] of the keys (the shape table rehashes with it).
    pub(crate) hash: u64,
}

impl Shape {
    /// A shape over `keys` (unique, in order) owned by the shape table `owner`.
    pub(crate) fn new(keys: Box<[CompactString]>, owner: u64) -> Self {
        let index = (keys.len() > WIDE_SHAPE).then(|| keys.iter().cloned().collect());
        let prints = keys.iter().map(|key| KeyPrint::of(key)).collect();
        let hash = keys_hash(keys.iter().map(CompactString::as_str));
        Self {
            keys,
            prints,
            index,
            owner,
            hash,
        }
    }

    /// The position of `key`, if present.
    #[inline]
    pub(crate) fn position(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(index) => index.get_index_of(key),
            None => self.position_of_print(KeyPrint::of(key), key),
        }
    }

    /// The position of `key` whose print is `print`, by a scan of the prints.
    #[inline]
    fn position_of_print(&self, print: KeyPrint, key: &str) -> Option<usize> {
        self.prints
            .iter()
            .zip(self.keys.iter())
            .position(|(candidate, stored)| *candidate == print && print.confirms(stored, key))
    }

    /// Whether the keys of this shape are exactly `keys`, in order.
    #[inline]
    pub(crate) fn has_keys<'a>(&self, mut keys: impl ExactSizeIterator<Item = &'a str>) -> bool {
        keys.len() == self.keys.len()
            && self
                .prints
                .iter()
                .zip(self.keys.iter())
                .all(|(print, stored)| {
                    keys.next().is_some_and(|key| {
                        let other = KeyPrint::of(key);
                        *print == other && other.confirms(stored, key)
                    })
                })
    }

    /// The number of keys.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether this shape keeps a hash index of its keys (tests).
    #[cfg(test)]
    pub(crate) fn is_wide(&self) -> bool {
        self.index.is_some()
    }

    /// The bytes of this shape: its `Arc` allocation (two counters plus the struct), the key
    /// list and its prints, each heap-allocated key, and a wide shape's index (whose keys are
    /// clones: a long key's heap counted again).
    pub(crate) fn heap_bytes(&self) -> u64 {
        use std::mem::size_of;
        let arc = (2 * size_of::<usize>() + size_of::<Self>()) as u64;
        let keys =
            self.keys.len() as u64 * (size_of::<CompactString>() + size_of::<KeyPrint>()) as u64;
        let key_heap: u64 = self.keys.iter().map(super::compact_string_heap_bytes).sum();
        let index = self.index.as_ref().map_or(0, |index| {
            index_map_heap_bytes::<CompactString, ()>(index.capacity()) + key_heap
        });
        arc + keys + key_heap + index
    }
}

/// The parts of a `Shaped` [`PropertyMap`] (crate-internal): the shared shape and the values in
/// its key order. Invariant: `values.len() == shape.len()`.
pub(crate) struct Shaped {
    /// The key list.
    pub(crate) shape: Arc<Shape>,
    /// The values, one per key, in key order. Exactly sized when built or grown (an added key
    /// reserves exactly one slot); a removed key keeps its slot as capacity, so a drop followed by
    /// an add (or a rollback of either) reallocates nothing.
    pub(crate) values: Vec<ElementProperty>,
}

/// The representation of a [`PropertyMap`] (crate-internal; see the module docs).
pub(crate) enum Repr {
    /// A free-standing map.
    Indexed(PropertyMapInner),
    /// A map stored in a `TraversalGraph`: shared keys, own values.
    Shaped(Shaped),
}

impl Default for Repr {
    #[inline]
    fn default() -> Self {
        Repr::Indexed(PropertyMapInner::default())
    }
}

/// A graph element's property map: property name to [`ElementProperty`] value, in insertion order.
///
/// It is the payload of [`ElementProperty::Object`], [`ElementProperty::Vertex`] and
/// [`ElementProperty::Edge`], and the property map a [`GraphStorage`](crate::graph::GraphStorage)
/// stores per element. Keys are [`CompactString`]s; every method that looks a key up takes a
/// `&str`. Equality ignores the order of the entries.
///
/// ```
/// use graphersal::prelude::{ElementProperty, PropertyMap};
///
/// let mut props = PropertyMap::new();
/// props.insert("name", ElementProperty::from("marko"));
/// props.insert("age", ElementProperty::from(29));
/// assert_eq!(props.len(), 2);
/// assert_eq!(props["name"], ElementProperty::from("marko"));
/// assert_eq!(props.keys().map(|k| k.as_str()).collect::<Vec<_>>(), ["name", "age"]);
/// ```
#[derive(Default)]
pub struct PropertyMap(pub(crate) Repr);

impl Eq for PropertyMap {}

// `Clone` and `PartialEq` recurse through nested values (`ElementProperty::Object` holds a
// `PropertyMap`); `#[inline(always)]` keeps the wrapper from adding a stack frame per nesting
// level in unoptimized builds, where the deepest values (`traversal.max_value_depth`) run close to
// the 2 MiB test-thread stack. The match is written out here (no helper call), and nested maps are
// always `Indexed`, so the recursion goes through the same frames as a plain `IndexMap`.
impl Clone for PropertyMap {
    #[inline(always)]
    fn clone(&self) -> Self {
        match &self.0 {
            Repr::Indexed(map) => Self(Repr::Indexed(map.clone())),
            Repr::Shaped(shaped) => Self(Repr::Shaped(Shaped {
                shape: Arc::clone(&shaped.shape),
                values: shaped.values.clone(),
            })),
        }
    }

    #[inline(always)]
    fn clone_from(&mut self, source: &Self) {
        match (&mut self.0, &source.0) {
            (Repr::Indexed(target), Repr::Indexed(source)) => target.clone_from(source),
            (target, _) => *target = source.clone().0,
        }
    }
}

impl PartialEq for PropertyMap {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Indexed(a), Repr::Indexed(b)) => a == b,
            _ => self.eq_mixed(other),
        }
    }
}

impl PropertyMap {
    /// An empty map.
    #[inline]
    pub fn new() -> Self {
        Self(Repr::Indexed(PropertyMapInner::default()))
    }

    /// An empty map with room for `capacity` entries.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Repr::Indexed(PropertyMapInner::with_capacity_and_hasher(
            capacity,
            BuildHasherDefault::default(),
        )))
    }

    /// The number of entries.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Indexed(map) => map.len(),
            Repr::Shaped(shaped) => shaped.values.len(),
        }
    }

    /// Whether the map has no entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The value of `key`, if present.
    #[inline]
    pub fn get(&self, key: &str) -> Option<&ElementProperty> {
        match &self.0 {
            Repr::Indexed(map) => map.get(key),
            Repr::Shaped(shaped) => shaped
                .shape
                .position(key)
                .and_then(|position| shaped.values.get(position)),
        }
    }

    /// The value of `key`, mutably, if present.
    #[inline]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut ElementProperty> {
        match &mut self.0 {
            Repr::Indexed(map) => map.get_mut(key),
            Repr::Shaped(shaped) => shaped
                .shape
                .position(key)
                .and_then(|position| shaped.values.get_mut(position)),
        }
    }

    /// Whether `key` is present.
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        match &self.0 {
            Repr::Indexed(map) => map.contains_key(key),
            Repr::Shaped(shaped) => shaped.shape.position(key).is_some(),
        }
    }

    /// Inserts `value` under `key` and returns the previous value. A new key goes to the end; an
    /// existing key keeps its position.
    #[inline]
    pub fn insert(
        &mut self,
        key: impl Into<CompactString>,
        value: ElementProperty,
    ) -> Option<ElementProperty> {
        match &mut self.0 {
            Repr::Indexed(map) => map.insert(key.into(), value),
            Repr::Shaped(_) => self.insert_full_shaped(key.into(), value).1,
        }
    }

    /// Removes `key` and returns its value, keeping the order of the other entries.
    #[inline]
    pub fn shift_remove(&mut self, key: &str) -> Option<ElementProperty> {
        match &mut self.0 {
            Repr::Indexed(map) => map.shift_remove(key),
            Repr::Shaped(_) => self.shift_remove_full(key).map(|(_, _, value)| value),
        }
    }

    /// The entries in order.
    #[inline]
    pub fn iter(&self) -> Iter<'_> {
        Iter(match &self.0 {
            Repr::Indexed(map) => Either::Indexed(map.iter()),
            Repr::Shaped(shaped) => {
                Either::Shaped(shaped.shape.keys.iter().zip(shaped.values.iter()))
            }
        })
    }

    /// The entries in order, values mutable.
    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        IterMut(match &mut self.0 {
            Repr::Indexed(map) => Either::Indexed(map.iter_mut()),
            Repr::Shaped(shaped) => {
                Either::Shaped(shaped.shape.keys.iter().zip(shaped.values.iter_mut()))
            }
        })
    }

    /// The keys in order.
    #[inline]
    pub fn keys(&self) -> Keys<'_> {
        Keys(match &self.0 {
            Repr::Indexed(map) => Either::Indexed(map.keys()),
            Repr::Shaped(shaped) => Either::Shaped(shaped.shape.keys.iter()),
        })
    }

    /// The values in order.
    #[inline]
    pub fn values(&self) -> Values<'_> {
        Values(match &self.0 {
            Repr::Indexed(map) => Either::Indexed(map.values()),
            Repr::Shaped(shaped) => Either::Shaped(shaped.values.iter()),
        })
    }

    /// The values in order, mutable.
    #[inline]
    pub fn values_mut(&mut self) -> ValuesMut<'_> {
        ValuesMut(match &mut self.0 {
            Repr::Indexed(map) => Either::Indexed(map.values_mut()),
            Repr::Shaped(shaped) => Either::Shaped(shaped.values.iter_mut()),
        })
    }

    /// Removes every entry, keeping the allocated capacity.
    #[inline]
    pub fn clear(&mut self) {
        match &mut self.0 {
            Repr::Indexed(map) => map.clear(),
            Repr::Shaped(_) => self.0 = Repr::default(),
        }
    }
}

/// Crate-internal operations: positions (the undo log restores a key at its old position), the
/// representation, and the heap estimate.
impl PropertyMap {
    /// Order-independent equality across representations (the same rule as `IndexMap`'s).
    #[inline(never)]
    fn eq_mixed(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(key, value)| other.get(key).is_some_and(|other| value == other))
    }

    /// The `Indexed` map, converting a `Shaped` one first (its keys copied out of the shape):
    /// the step before a key-changing mutation outside the graph's shape table.
    #[inline]
    pub(crate) fn indexed_mut(&mut self) -> &mut PropertyMapInner {
        if let Repr::Shaped(_) = self.0 {
            self.unshape();
        }
        match self.0 {
            Repr::Indexed(ref mut map) => map,
            // Not reached: converted above. Starting over from an empty map keeps this total.
            Repr::Shaped(_) => {
                self.0 = Repr::default();
                self.indexed_mut()
            }
        }
    }

    /// Converts a `Shaped` map to an `Indexed` one (kept out of line: the free-standing maps the
    /// hot paths build never take it).
    #[cold]
    #[inline(never)]
    fn unshape(&mut self) {
        if let Repr::Shaped(shaped) = &mut self.0 {
            let values = std::mem::take(&mut shaped.values);
            let map: PropertyMapInner = shaped.shape.keys.iter().cloned().zip(values).collect();
            self.0 = Repr::Indexed(map);
        }
    }

    /// A `Shaped` map over `shape` with `values` in its key order (a decoder builds one with a
    /// shape of its own; the graph adopts it when the element is stored). The value count must
    /// equal the key count; extra values are dropped, missing ones make the map shorter.
    pub(crate) fn from_shaped(shape: Arc<Shape>, mut values: Vec<ElementProperty>) -> Self {
        if values.len() != shape.len() {
            values.truncate(shape.len());
            if values.len() < shape.len() {
                let keys = shape.keys.iter().cloned().zip(values).collect();
                return Self(Repr::Indexed(keys));
            }
        }
        Self(Repr::Shaped(Shaped { shape, values }))
    }

    /// The `Indexed` map, if this map is one (tests).
    #[cfg(test)]
    pub(crate) fn as_indexed(&self) -> Option<&PropertyMapInner> {
        match &self.0 {
            Repr::Indexed(map) => Some(map),
            Repr::Shaped(_) => None,
        }
    }

    /// The shape and the values of a `Shaped` map (the memory report aggregates by shape).
    pub(crate) fn shaped_parts(&self) -> Option<(&Arc<Shape>, &[ElementProperty])> {
        match &self.0 {
            Repr::Indexed(_) => None,
            Repr::Shaped(shaped) => Some((&shaped.shape, &shaped.values)),
        }
    }

    /// The shape of a `Shaped` map (tests).
    #[cfg(test)]
    pub(crate) fn shape(&self) -> Option<&Arc<Shape>> {
        match &self.0 {
            Repr::Indexed(_) => None,
            Repr::Shaped(shaped) => Some(&shaped.shape),
        }
    }

    /// `insert` with the position: `(position, previous value)`. An existing key keeps its
    /// position and its representation; a new key on a `Shaped` map converts it to `Indexed`.
    #[inline]
    pub(crate) fn insert_full(
        &mut self,
        key: CompactString,
        value: ElementProperty,
    ) -> (usize, Option<ElementProperty>) {
        match &mut self.0 {
            Repr::Indexed(map) => map.insert_full(key, value),
            Repr::Shaped(_) => self.insert_full_shaped(key, value),
        }
    }

    /// [`Self::insert_full`] of a `Shaped` map: an existing key in place, a new one after the
    /// conversion to `Indexed`.
    #[inline(never)]
    fn insert_full_shaped(
        &mut self,
        key: CompactString,
        value: ElementProperty,
    ) -> (usize, Option<ElementProperty>) {
        if let Repr::Shaped(shaped) = &mut self.0
            && let Some(position) = shaped.shape.position(&key)
            && let Some(slot) = shaped.values.get_mut(position)
        {
            return (position, Some(std::mem::replace(slot, value)));
        }
        self.indexed_mut().insert_full(key, value)
    }

    /// `shift_remove` with the position and the stored key. An absent key changes nothing.
    pub(crate) fn shift_remove_full(
        &mut self,
        key: &str,
    ) -> Option<(usize, CompactString, ElementProperty)> {
        match &mut self.0 {
            Repr::Indexed(map) => map.shift_remove_full(key),
            Repr::Shaped(shaped) => {
                shaped.shape.position(key)?;
                self.indexed_mut().shift_remove_full(key)
            }
        }
    }

    /// The position, stored key and value of `key`.
    pub(crate) fn get_full(&self, key: &str) -> Option<(usize, &CompactString, &ElementProperty)> {
        match &self.0 {
            Repr::Indexed(map) => map.get_full(key),
            Repr::Shaped(shaped) => {
                let position = shaped.shape.position(key)?;
                Some((
                    position,
                    shaped.shape.keys.get(position)?,
                    shaped.values.get(position)?,
                ))
            }
        }
    }

    /// Inserts `key` (absent) at `position` (clamped to the length).
    pub(crate) fn shift_insert(
        &mut self,
        position: usize,
        key: CompactString,
        value: ElementProperty,
    ) {
        let map = self.indexed_mut();
        let position = position.min(map.len());
        map.shift_insert(position, key, value);
    }

    /// Inserts `key` with the value `make` returns, only when `key` is absent.
    pub(crate) fn insert_if_absent(
        &mut self,
        key: &CompactString,
        make: impl FnOnce() -> ElementProperty,
    ) {
        if !self.contains_key(key) {
            self.indexed_mut().insert(key.clone(), make());
        }
    }

    /// The heap bytes of the map's own storage, without the values' heap: for an `Indexed` map
    /// its two allocations sized from its capacity ([`index_map_heap_bytes`]) and each
    /// heap-allocated key; for a `Shaped` map its value slice alone (the shared shape is counted
    /// once, by the shape table: `MemoryStats::shape_bytes`).
    pub(crate) fn own_heap_bytes(&self) -> u64 {
        match &self.0 {
            Repr::Indexed(map) => {
                index_map_heap_bytes::<CompactString, ElementProperty>(map.capacity())
                    + map
                        .keys()
                        .map(super::compact_string_heap_bytes)
                        .sum::<u64>()
            }
            Repr::Shaped(shaped) => shaped_values_bytes(shaped.values.capacity()),
        }
    }
}

/// The bytes of a `Shaped` map's value slice of `len` values.
pub(crate) fn shaped_values_bytes(len: usize) -> u64 {
    len as u64 * std::mem::size_of::<ElementProperty>() as u64
}

impl fmt::Debug for PropertyMap {
    /// The text of the `IndexMap` debug form (`{"k": v, ...}` in order) for both representations.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// Looks a key up; panics when it is absent, like the indexing of the standard maps (use
/// [`PropertyMap::get`] for a fallible lookup).
impl std::ops::Index<&str> for PropertyMap {
    type Output = ElementProperty;

    #[inline]
    fn index(&self, key: &str) -> &ElementProperty {
        match self.get(key) {
            Some(value) => value,
            None => panic!("PropertyMap: key not found"),
        }
    }
}

impl<K: Into<CompactString>> FromIterator<(K, ElementProperty)> for PropertyMap {
    #[inline]
    fn from_iter<I: IntoIterator<Item = (K, ElementProperty)>>(iter: I) -> Self {
        Self(Repr::Indexed(
            iter.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        ))
    }
}

impl<K: Into<CompactString>> Extend<(K, ElementProperty)> for PropertyMap {
    #[inline]
    fn extend<I: IntoIterator<Item = (K, ElementProperty)>>(&mut self, iter: I) {
        self.indexed_mut()
            .extend(iter.into_iter().map(|(k, v)| (k.into(), v)));
    }
}

impl<K: Into<CompactString>, const N: usize> From<[(K, ElementProperty); N]> for PropertyMap {
    #[inline]
    fn from(entries: [(K, ElementProperty); N]) -> Self {
        entries.into_iter().collect()
    }
}

impl IntoIterator for PropertyMap {
    type Item = (CompactString, ElementProperty);
    type IntoIter = IntoIter;

    #[inline]
    fn into_iter(self) -> IntoIter {
        IntoIter(match self.0 {
            Repr::Indexed(map) => Either::Indexed(map.into_iter()),
            Repr::Shaped(Shaped { shape, values }) => Either::Shaped(ShapedIntoIter {
                keys: 0..shape.len(),
                shape,
                values: values.into_iter(),
            }),
        })
    }
}

impl<'a> IntoIterator for &'a PropertyMap {
    type Item = (&'a CompactString, &'a ElementProperty);
    type IntoIter = Iter<'a>;

    #[inline]
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut PropertyMap {
    type Item = (&'a CompactString, &'a mut ElementProperty);
    type IntoIter = IterMut<'a>;

    #[inline]
    fn into_iter(self) -> IterMut<'a> {
        self.iter_mut()
    }
}

/// The iterator of one representation or the other (crate-internal).
#[derive(Clone)]
enum Either<I, S> {
    Indexed(I),
    Shaped(S),
}

impl<T, I: Iterator<Item = T>, S: Iterator<Item = T>> Iterator for Either<I, S> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<T> {
        match self {
            Either::Indexed(iter) => iter.next(),
            Either::Shaped(iter) => iter.next(),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Either::Indexed(iter) => iter.size_hint(),
            Either::Shaped(iter) => iter.size_hint(),
        }
    }
}

impl<T, I: DoubleEndedIterator<Item = T>, S: DoubleEndedIterator<Item = T>> DoubleEndedIterator
    for Either<I, S>
{
    #[inline]
    fn next_back(&mut self) -> Option<T> {
        match self {
            Either::Indexed(iter) => iter.next_back(),
            Either::Shaped(iter) => iter.next_back(),
        }
    }
}

/// The owned entries of a `Shaped` map: the keys are cloned out of the shared shape.
struct ShapedIntoIter {
    shape: Arc<Shape>,
    keys: std::ops::Range<usize>,
    values: std::vec::IntoIter<ElementProperty>,
}

impl Iterator for ShapedIntoIter {
    type Item = (CompactString, ElementProperty);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let value = self.values.next()?;
        let key = self.shape.keys.get(self.keys.next()?)?;
        Some((key.clone(), value))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}

impl DoubleEndedIterator for ShapedIntoIter {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        let value = self.values.next_back()?;
        let key = self.shape.keys.get(self.keys.next_back()?)?;
        Some((key.clone(), value))
    }
}

/// Implements the iterator traits of a public wrapper around an [`Either`] iterator.
macro_rules! wrap_iter {
    ($name:ident $(<$lt:lifetime>)?, $item:ty) => {
        impl$(<$lt>)? Iterator for $name$(<$lt>)? {
            type Item = $item;

            #[inline]
            fn next(&mut self) -> Option<$item> {
                self.0.next()
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                self.0.size_hint()
            }
        }

        impl$(<$lt>)? DoubleEndedIterator for $name$(<$lt>)? {
            #[inline]
            fn next_back(&mut self) -> Option<$item> {
                self.0.next_back()
            }
        }

        impl$(<$lt>)? ExactSizeIterator for $name$(<$lt>)? {}

        impl$(<$lt>)? FusedIterator for $name$(<$lt>)? {}
    };
}

type KeySlice<'a> = std::slice::Iter<'a, CompactString>;

/// The entries of a [`PropertyMap`] in order ([`PropertyMap::iter`]).
#[derive(Clone)]
pub struct Iter<'a>(
    Either<
        indexmap::map::Iter<'a, CompactString, ElementProperty>,
        std::iter::Zip<KeySlice<'a>, std::slice::Iter<'a, ElementProperty>>,
    >,
);
wrap_iter!(Iter<'a>, (&'a CompactString, &'a ElementProperty));

impl fmt::Debug for Iter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

/// The entries of a [`PropertyMap`] in order, values mutable ([`PropertyMap::iter_mut`]).
pub struct IterMut<'a>(
    Either<
        indexmap::map::IterMut<'a, CompactString, ElementProperty>,
        std::iter::Zip<KeySlice<'a>, std::slice::IterMut<'a, ElementProperty>>,
    >,
);
wrap_iter!(IterMut<'a>, (&'a CompactString, &'a mut ElementProperty));

impl fmt::Debug for IterMut<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IterMut")
            .field("remaining", &self.len())
            .finish()
    }
}

/// The keys of a [`PropertyMap`] in order ([`PropertyMap::keys`]).
#[derive(Clone)]
pub struct Keys<'a>(Either<indexmap::map::Keys<'a, CompactString, ElementProperty>, KeySlice<'a>>);
wrap_iter!(Keys<'a>, &'a CompactString);

impl fmt::Debug for Keys<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

/// The values of a [`PropertyMap`] in order ([`PropertyMap::values`]).
#[derive(Clone)]
pub struct Values<'a>(
    Either<
        indexmap::map::Values<'a, CompactString, ElementProperty>,
        std::slice::Iter<'a, ElementProperty>,
    >,
);
wrap_iter!(Values<'a>, &'a ElementProperty);

impl fmt::Debug for Values<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

/// The values of a [`PropertyMap`] in order, mutable ([`PropertyMap::values_mut`]).
pub struct ValuesMut<'a>(
    Either<
        indexmap::map::ValuesMut<'a, CompactString, ElementProperty>,
        std::slice::IterMut<'a, ElementProperty>,
    >,
);
wrap_iter!(ValuesMut<'a>, &'a mut ElementProperty);

impl fmt::Debug for ValuesMut<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValuesMut")
            .field("remaining", &self.len())
            .finish()
    }
}

/// The entries of a [`PropertyMap`] by value, in order (its `IntoIterator`).
pub struct IntoIter(
    Either<indexmap::map::IntoIter<CompactString, ElementProperty>, ShapedIntoIter>,
);
wrap_iter!(IntoIter, (CompactString, ElementProperty));

impl fmt::Debug for IntoIter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntoIter")
            .field("remaining", &self.len())
            .finish()
    }
}

/// The control-group width of the `hashbrown` table behind an `IndexMap`'s index: 16 with SSE2
/// on x86/x86_64, 8 elsewhere on 64-bit targets, aarch64 and wasm32 (NEON or a `u64` word), 4 on
/// other 32-bit targets (a `u32` word). Mirrors `hashbrown` 0.17's `Group::WIDTH`, which is not
/// public.
const HASH_GROUP_WIDTH: usize = if cfg!(all(
    target_feature = "sse2",
    any(target_arch = "x86", target_arch = "x86_64")
)) {
    16
} else if cfg!(any(
    target_pointer_width = "64",
    target_arch = "aarch64",
    target_arch = "wasm32"
)) {
    8
} else {
    4
};

/// The bucket count of the `hashbrown` table that holds `capacity` items: `0` for no allocation,
/// 4 or 8 or 16 for small tables, else the next power of two of `capacity * 8 / 7` (load factor
/// 7/8). Mirrors `hashbrown` 0.17's `capacity_to_buckets` for `usize` items (whose size never
/// triggers its raised minimum for one- to three-byte items).
fn hash_table_buckets(capacity: usize) -> u64 {
    match capacity {
        0 => 0,
        1..=3 => 4,
        4..=7 => 8,
        8..=14 => 16,
        _ => (capacity.saturating_mul(8) / 7).next_power_of_two() as u64,
    }
}

/// The heap bytes of an `IndexMap<K, V, _>` (or an `IndexSet<K>` with `V = ()`) whose
/// [`IndexMap::capacity`] is `capacity`. An `IndexMap` owns two allocations:
///
/// - the entries, a `Vec` of buckets, each the cached hash (`usize`), the key and the value:
///   `capacity * size_of::<(usize, K, V)>()`;
/// - the index, a `hashbrown` table of `usize` positions: `buckets * size_of::<usize>()` (rounded
///   up to the control alignment) plus one control byte per bucket and one trailing group
///   (`buckets + Group::WIDTH`).
///
/// Neither `indexmap` nor `hashbrown` exposes the index table's bucket count, so it is derived
/// from `capacity` with `hashbrown`'s own formula ([`hash_table_buckets`]). `IndexMap::capacity`
/// is the smaller of the two allocations' capacities; `indexmap` grows the entries to match the
/// index, so the two agree for a map built by insertion. A map that never allocated (`capacity`
/// 0) owns nothing. Every size comes from `size_of`, so the figure is right on 32-bit targets
/// (wasm32) as well; allocator rounding and bookkeeping are not counted.
pub(crate) fn index_map_heap_bytes<K, V>(capacity: usize) -> u64 {
    use std::mem::{align_of, size_of};
    let buckets = hash_table_buckets(capacity);
    if buckets == 0 {
        return 0;
    }
    let entries = capacity as u64 * size_of::<(usize, K, V)>() as u64;
    let ctrl_align = align_of::<usize>().max(HASH_GROUP_WIDTH) as u64;
    let slots = (buckets * size_of::<usize>() as u64).next_multiple_of(ctrl_align);
    entries + slots + buckets + HASH_GROUP_WIDTH as u64
}

/// The heap bytes of a `hashbrown` table (a `std` `HashMap`/`HashSet`) whose `.capacity()` is
/// `capacity` and whose entries are `entry` bytes: the bucket array (rounded up to the control
/// alignment) plus one control byte per bucket and one trailing group. A table that never
/// allocated owns nothing.
pub(crate) fn hash_table_heap_bytes(capacity: usize, entry: usize) -> u64 {
    use std::mem::align_of;
    let buckets = hash_table_buckets(capacity);
    if buckets == 0 {
        return 0;
    }
    let ctrl_align = align_of::<usize>().max(HASH_GROUP_WIDTH) as u64;
    (buckets * entry as u64).next_multiple_of(ctrl_align) + buckets + HASH_GROUP_WIDTH as u64
}

#[cfg(feature = "serde")]
mod serde_impl {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{PropertyMap, PropertyMapInner, Repr};

    /// Serializes as a JSON object, entries in order.
    impl Serialize for PropertyMap {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_map(self.iter())
        }
    }

    /// Deserializes a JSON object, entries in order.
    impl<'de> Deserialize<'de> for PropertyMap {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            PropertyMapInner::deserialize(deserializer).map(|map| PropertyMap(Repr::Indexed(map)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PropertyMap, PropertyMapInner};
    use crate::value::ElementProperty;

    #[test]
    fn the_newtype_costs_nothing() {
        assert_eq!(size_of::<PropertyMap>(), size_of::<PropertyMapInner>());
        assert_eq!(align_of::<PropertyMap>(), align_of::<PropertyMapInner>());
        assert_eq!(size_of::<ElementProperty>(), 32);
    }

    #[test]
    fn keeps_insertion_order_and_compares_unordered() {
        let mut a = PropertyMap::new();
        a.insert("b", ElementProperty::from(1));
        a.insert("a", ElementProperty::from(2));
        a.insert("b", ElementProperty::from(3));
        assert_eq!(a.keys().map(|k| k.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(a["b"], ElementProperty::from(3));
        let b: PropertyMap = [
            ("a", ElementProperty::from(2)),
            ("b", ElementProperty::from(3)),
        ]
        .into_iter()
        .collect();
        assert_eq!(a, b);
        assert_eq!(a.shift_remove("b"), Some(ElementProperty::from(3)));
        assert!(!a.contains_key("b"));
        a.extend([("c", ElementProperty::Null)]);
        for value in a.values_mut() {
            *value = ElementProperty::Null;
        }
        let owned: Vec<_> = a.clone().into_iter().collect();
        assert_eq!(owned.len(), 2);
        assert_eq!((&a).into_iter().len(), 2);
        a.clear();
        assert!(a.is_empty());
    }

    use super::{HASH_GROUP_WIDTH, hash_table_buckets, index_map_heap_bytes};
    use compact_str::CompactString;

    /// The table bytes `index_map_heap_bytes` adds for `buckets` buckets.
    fn table(buckets: u64) -> u64 {
        let align = align_of::<usize>().max(HASH_GROUP_WIDTH) as u64;
        (buckets * size_of::<usize>() as u64).next_multiple_of(align)
            + buckets
            + HASH_GROUP_WIDTH as u64
    }

    #[test]
    fn an_empty_map_owns_no_heap() {
        let map = PropertyMapInner::default();
        assert_eq!(map.capacity(), 0);
        assert_eq!(
            index_map_heap_bytes::<CompactString, ElementProperty>(map.capacity()),
            0
        );
        assert_eq!(
            crate::value::estimated_element_heap_bytes(&PropertyMap::new()),
            0
        );
    }

    #[test]
    fn buckets_follow_hashbrown() {
        let expected = [
            (1, 4),
            (3, 4),
            (4, 8),
            (7, 8),
            (8, 16),
            (14, 16),
            (15, 32),
            (28, 32),
            (29, 64),
            (56, 64),
            (57, 128),
            (1000, 2048),
        ];
        for (capacity, buckets) in expected {
            assert_eq!(hash_table_buckets(capacity), buckets, "capacity {capacity}");
        }
    }

    /// `with_capacity(n)` gives the entries exactly `n` and the index at least `n`, so
    /// `capacity()` is `n` and the estimate is `n` buckets plus the index table of `n` items.
    #[test]
    fn the_estimate_of_a_presized_map_is_entries_plus_index() {
        let bucket = size_of::<(usize, CompactString, ElementProperty)>() as u64;
        #[cfg(target_pointer_width = "64")]
        assert_eq!(bucket, 64);
        for (n, buckets) in [
            (1u64, 4),
            (3, 4),
            (5, 8),
            (7, 8),
            (10, 16),
            (14, 16),
            (100, 128),
        ] {
            let map = PropertyMapInner::with_capacity_and_hasher(n as usize, Default::default());
            assert_eq!(map.capacity(), n as usize);
            assert_eq!(
                index_map_heap_bytes::<CompactString, ElementProperty>(map.capacity()),
                n * bucket + table(buckets),
                "n = {n}"
            );
        }
    }

    /// A map grown by insertion: its entries follow the index capacity (3, 7, 14, 28, ...), and
    /// the estimate is never below the plain entry bytes.
    #[test]
    fn the_estimate_of_a_grown_map_counts_the_index() {
        let bucket = size_of::<(usize, CompactString, ElementProperty)>() as u64;
        for (len, capacity, buckets) in [(1, 3, 4), (3, 3, 4), (5, 7, 8), (8, 14, 16), (20, 28, 32)]
        {
            let mut map = PropertyMap::new();
            for key in 0..len {
                map.insert(format!("k{key}"), ElementProperty::from(key));
            }
            assert_eq!(
                map.as_indexed().map(|m| m.capacity()),
                Some(capacity),
                "len {len}"
            );
            assert_eq!(
                crate::value::estimated_element_heap_bytes(&map),
                capacity as u64 * bucket + table(buckets),
                "len {len}"
            );
        }
    }

    /// Nested maps count their box, their own table and their heap strings.
    #[test]
    fn a_nested_object_counts_its_box_and_table() {
        let mut inner = PropertyMap::new();
        inner.insert(
            "x",
            ElementProperty::from("a string far longer than the inline limit"),
        );
        let inner_bytes = crate::value::estimated_element_heap_bytes(&inner);
        assert!(inner_bytes > index_map_heap_bytes::<CompactString, ElementProperty>(3));
        let nested = ElementProperty::Object(Box::new(inner));
        assert_eq!(
            nested.estimated_heap_bytes(),
            size_of::<PropertyMap>() as u64 + inner_bytes
        );
    }
}

/// The semantics of `PropertyMap` as callers observe them today, pinned before its
/// representation changes: order rules, order-independent equality, `Debug` text, `Clone`, the
/// auto traits, serde, and wide maps.
#[cfg(test)]
mod semantics_tests {
    use super::{IntoIter, Iter, IterMut, Keys, PropertyMap, Values, ValuesMut};
    use crate::value::ElementProperty;

    fn keys(map: &PropertyMap) -> Vec<&str> {
        map.keys().map(|k| k.as_str()).collect()
    }

    fn owned_keys(map: &PropertyMap) -> Vec<String> {
        map.keys().map(|k| k.to_string()).collect()
    }

    fn sample() -> PropertyMap {
        PropertyMap::from([
            ("name", ElementProperty::from("marko")),
            ("age", ElementProperty::from(29)),
            ("ok", ElementProperty::from(true)),
        ])
    }

    #[test]
    fn insert_overwrite_remove_and_readd_follow_the_order_rules() {
        let mut map = sample();
        assert_eq!(keys(&map), ["name", "age", "ok"]);
        // Overwrite: same position, previous value returned.
        assert_eq!(
            map.insert("name", ElementProperty::from("vadas")),
            Some(ElementProperty::from("marko"))
        );
        assert_eq!(keys(&map), ["name", "age", "ok"]);
        // New key: the end.
        assert_eq!(map.insert("city", ElementProperty::Null), None);
        assert_eq!(keys(&map), ["name", "age", "ok", "city"]);
        // Remove the first, a middle one: the rest keep their order.
        assert_eq!(
            map.shift_remove("name"),
            Some(ElementProperty::from("vadas"))
        );
        assert_eq!(map.shift_remove("ok"), Some(ElementProperty::from(true)));
        assert_eq!(map.shift_remove("absent"), None);
        assert_eq!(keys(&map), ["age", "city"]);
        // Re-added: the end.
        map.insert("name", ElementProperty::from(1));
        assert_eq!(keys(&map), ["age", "city", "name"]);
        // `get_mut`, `iter_mut` and `values_mut` change values, never the order.
        *map.get_mut("age").unwrap() = ElementProperty::from(30);
        assert!(map.get_mut("absent").is_none());
        for (key, value) in map.iter_mut() {
            if key == "city" {
                *value = ElementProperty::from("rome");
            }
        }
        for value in map.values_mut() {
            if *value == ElementProperty::from(1) {
                *value = ElementProperty::from(2);
            }
        }
        assert_eq!(
            map.iter()
                .map(|(k, v)| (k.as_str(), v.clone()))
                .collect::<Vec<_>>(),
            [
                ("age", ElementProperty::from(30)),
                ("city", ElementProperty::from("rome")),
                ("name", ElementProperty::from(2)),
            ]
        );
        assert_eq!(
            map.values().cloned().collect::<Vec<_>>(),
            [
                ElementProperty::from(30),
                ElementProperty::from("rome"),
                ElementProperty::from(2)
            ]
        );
        // `clear` empties; a later insert starts a fresh order.
        map.clear();
        assert!(map.is_empty());
        map.insert("z", ElementProperty::Null);
        map.insert("a", ElementProperty::Null);
        assert_eq!(keys(&map), ["z", "a"]);
    }

    #[test]
    fn iterators_are_ordered_double_ended_and_exact() {
        let mut map = sample();
        assert_eq!(map.iter().len(), 3);
        assert_eq!(
            map.keys().rev().map(|k| k.as_str()).collect::<Vec<_>>(),
            ["ok", "age", "name"]
        );
        assert_eq!(map.values().next_back(), Some(&ElementProperty::from(true)));
        let mut iter = map.iter();
        assert_eq!(iter.size_hint(), (3, Some(3)));
        assert_eq!(iter.next().map(|(k, _)| k.as_str()), Some("name"));
        assert_eq!(iter.next_back().map(|(k, _)| k.as_str()), Some("ok"));
        assert_eq!(iter.len(), 1);
        assert_eq!(iter.next().map(|(k, _)| k.as_str()), Some("age"));
        assert!(iter.next().is_none());
        assert!(iter.next().is_none(), "fused");
        assert_eq!(map.iter_mut().len(), 3);
        assert_eq!(map.values_mut().rev().count(), 3);
        assert_eq!((&mut map).into_iter().len(), 3);
        assert_eq!(
            (&map)
                .into_iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>(),
            ["name", "age", "ok"]
        );
        let mut owned = map.clone().into_iter();
        assert_eq!(owned.len(), 3);
        assert_eq!(
            owned.next_back().map(|(k, _)| k.to_string()),
            Some("ok".into())
        );
        assert_eq!(
            owned.map(|(k, _)| k.to_string()).collect::<Vec<_>>(),
            ["name", "age"]
        );
    }

    #[test]
    fn construction_with_duplicate_keys_keeps_the_first_position_and_the_last_value() {
        let collected: PropertyMap = [
            ("b", ElementProperty::from(1)),
            ("a", ElementProperty::from(2)),
            ("b", ElementProperty::from(3)),
        ]
        .into_iter()
        .collect();
        assert_eq!(keys(&collected), ["b", "a"]);
        assert_eq!(collected["b"], ElementProperty::from(3));
        let from_array = PropertyMap::from([
            ("b", ElementProperty::from(1)),
            ("a", ElementProperty::from(2)),
            ("b", ElementProperty::from(3)),
        ]);
        assert_eq!(keys(&from_array), ["b", "a"]);
        assert_eq!(from_array["b"], ElementProperty::from(3));
        let mut extended = PropertyMap::from([("x", ElementProperty::Null)]);
        extended.extend([
            ("y", ElementProperty::from(1)),
            ("x", ElementProperty::from(2)),
            ("z", ElementProperty::from(3)),
        ]);
        assert_eq!(keys(&extended), ["x", "y", "z"]);
        assert_eq!(extended["x"], ElementProperty::from(2));
        let presized = PropertyMap::with_capacity(8);
        assert!(presized.is_empty());
        assert_eq!(presized, PropertyMap::new());
        assert_eq!(PropertyMap::default(), PropertyMap::new());
    }

    #[test]
    #[should_panic]
    fn indexing_an_absent_key_panics() {
        let _ = &sample()["absent"];
    }

    #[test]
    fn equality_ignores_order_but_not_keys_values_or_length() {
        let a = sample();
        let reversed: PropertyMap = a
            .iter()
            .rev()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(keys(&reversed), ["ok", "age", "name"]);
        assert_eq!(a, reversed);
        assert_eq!(reversed, a);
        let mut other_value = a.clone();
        other_value.insert("age", ElementProperty::from(30));
        assert_ne!(a, other_value);
        let mut longer = a.clone();
        longer.insert("extra", ElementProperty::Null);
        assert_ne!(a, longer);
        assert_ne!(longer, a);
        let mut other_key = a.clone();
        other_key.shift_remove("ok");
        other_key.insert("ko", ElementProperty::from(true));
        assert_ne!(a, other_key);
        // A removed and re-added key: other order, equal.
        let mut readded = a.clone();
        let value = readded.shift_remove("name").unwrap();
        readded.insert("name", value);
        assert_eq!(keys(&readded), ["age", "ok", "name"]);
        assert_eq!(a, readded);
        // Nested objects compare unordered as well.
        let nested = |map: PropertyMap| {
            PropertyMap::from([("inner", ElementProperty::Object(Box::new(map)))])
        };
        assert_eq!(nested(a.clone()), nested(reversed.clone()));
        assert_ne!(nested(a.clone()), nested(longer));
        assert_eq!(
            ElementProperty::Object(Box::new(a.clone())),
            ElementProperty::Object(Box::new(reversed))
        );
        // Empty maps are equal however they were made.
        let mut emptied = a.clone();
        emptied.clear();
        assert_eq!(emptied, PropertyMap::new());
        assert_eq!(PropertyMap::with_capacity(4), PropertyMap::new());
    }

    #[test]
    fn debug_is_the_map_text_in_order() {
        assert_eq!(format!("{:?}", PropertyMap::new()), "{}");
        assert_eq!(
            format!("{:?}", sample()),
            r#"{"name": "marko", "age": 29, "ok": true}"#
        );
        let nested = PropertyMap::from([
            ("z", ElementProperty::Null),
            ("o", ElementProperty::Object(Box::new(sample()))),
            (
                "l",
                ElementProperty::Array(vec![ElementProperty::from(1.5)]),
            ),
        ]);
        assert_eq!(
            format!("{nested:?}"),
            r#"{"z": null, "o": {"name": "marko", "age": 29, "ok": true}, "l": [1.5]}"#
        );
        assert_eq!(
            format!("{:#?}", sample()),
            "{\n    \"name\": \"marko\",\n    \"age\": 29,\n    \"ok\": true,\n}"
        );
        let map = sample();
        assert_eq!(
            format!("{:?}", map.iter()),
            r#"[("name", "marko"), ("age", 29), ("ok", true)]"#
        );
        assert_eq!(format!("{:?}", map.keys()), r#"["name", "age", "ok"]"#);
        assert_eq!(format!("{:?}", map.values()), r#"["marko", 29, true]"#);
        let mut map = map;
        assert_eq!(format!("{:?}", map.iter_mut()), "IterMut { remaining: 3 }");
        assert_eq!(
            format!("{:?}", map.values_mut()),
            "ValuesMut { remaining: 3 }"
        );
        assert_eq!(
            format!("{:?}", map.into_iter()),
            "IntoIter { remaining: 3 }"
        );
    }

    #[test]
    fn clone_keeps_order_and_is_independent() {
        let original = sample();
        let mut copy = original.clone();
        assert_eq!(keys(&copy), keys(&original));
        copy.insert("new", ElementProperty::Null);
        copy.shift_remove("name");
        assert_eq!(keys(&original), ["name", "age", "ok"]);
        assert_eq!(keys(&copy), ["age", "ok", "new"]);
        let mut target = PropertyMap::from([("zz", ElementProperty::Null)]);
        target.clone_from(&original);
        assert_eq!(keys(&target), ["name", "age", "ok"]);
        let mut wide = PropertyMap::new();
        for i in 0..40 {
            wide.insert(format!("k{i}"), ElementProperty::from(i));
        }
        target.clone_from(&wide);
        assert_eq!(owned_keys(&target), owned_keys(&wide));
        assert_eq!(target, wide);
    }

    #[test]
    fn wide_maps_find_insert_remove_and_keep_order() {
        for n in [20usize, 25, 100] {
            let order: Vec<usize> = (0..n).map(|i| (i * 37 + 11) % n).collect();
            let mut map = PropertyMap::new();
            for &i in &order {
                assert_eq!(map.insert(format!("k{i}"), ElementProperty::from(i)), None);
            }
            assert_eq!(map.len(), n);
            let expected: Vec<String> = order.iter().map(|i| format!("k{i}")).collect();
            assert_eq!(owned_keys(&map), expected);
            for &i in &order {
                assert_eq!(map.get(&format!("k{i}")), Some(&ElementProperty::from(i)));
                assert!(map.contains_key(&format!("k{i}")));
            }
            assert!(map.get("k-missing").is_none());
            // Overwrite all in reverse: order unchanged.
            for &i in order.iter().rev() {
                assert_eq!(
                    map.insert(format!("k{i}"), ElementProperty::Null),
                    Some(ElementProperty::from(i))
                );
            }
            assert_eq!(owned_keys(&map), expected);
            // Remove every third, re-add the first removed one: it goes to the end.
            let mut remaining = expected.clone();
            for key in expected.iter().step_by(3) {
                assert_eq!(map.shift_remove(key), Some(ElementProperty::Null));
                remaining.retain(|k| k != key);
            }
            map.insert(expected[0].as_str(), ElementProperty::from(1));
            remaining.push(expected[0].clone());
            assert_eq!(owned_keys(&map), remaining);
            assert_eq!(map.len(), remaining.len());
            // Equal to the same entries in sorted order.
            let mut sorted: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(sorted.into_iter().collect::<PropertyMap>(), map);
        }
    }

    /// `PropertyMap` implements no `Hash` today (`ElementProperty` holds floats). The check
    /// compiles only while that is so: a `Hash` type gets a second candidate impl of
    /// `AmbiguousIfHash`, and the inferred `_` becomes ambiguous.
    #[test]
    fn property_map_does_not_implement_hash() {
        trait AmbiguousIfHash<A> {
            fn some_item() {}
        }
        impl<T: ?Sized> AmbiguousIfHash<()> for T {}
        #[allow(dead_code)]
        struct Invalid;
        impl<T: ?Sized + std::hash::Hash> AmbiguousIfHash<Invalid> for T {}
        <PropertyMap as AmbiguousIfHash<_>>::some_item();
    }

    #[test]
    fn the_map_and_its_iterators_keep_their_auto_traits() {
        fn all_auto<T: Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe + Unpin>() {
        }
        all_auto::<PropertyMap>();
        all_auto::<Iter<'static>>();
        all_auto::<Keys<'static>>();
        all_auto::<Values<'static>>();
        all_auto::<IntoIter>();
        fn send_sync<T: Send + Sync + Unpin>() {}
        send_sync::<IterMut<'static>>();
        send_sync::<ValuesMut<'static>>();
        fn clone<T: Clone>() {}
        clone::<Iter<'static>>();
        clone::<Keys<'static>>();
        clone::<Values<'static>>();
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_is_a_json_object_in_order_and_round_trips() {
        let map = PropertyMap::from([
            ("zeta", ElementProperty::from(1)),
            ("alpha", ElementProperty::from("a")),
            (
                "mid",
                ElementProperty::Object(Box::new(PropertyMap::from([
                    ("y", ElementProperty::Null),
                    ("b", ElementProperty::from(true)),
                ]))),
            ),
        ]);
        let text = serde_json::to_string(&map).unwrap();
        assert_eq!(text, r#"{"zeta":1,"alpha":"a","mid":{"y":null,"b":true}}"#);
        let back: PropertyMap = serde_json::from_str(&text).unwrap();
        assert_eq!(keys(&back), ["zeta", "alpha", "mid"]);
        assert_eq!(back, map);
        assert_eq!(serde_json::to_string(&back).unwrap(), text);
        // Reading keeps the order of the document; a duplicate key keeps its first position.
        let read: PropertyMap = serde_json::from_str(r#"{"b":1,"a":2,"b":3}"#).unwrap();
        assert_eq!(keys(&read), ["b", "a"]);
        assert_eq!(read["b"], ElementProperty::from(3));
        assert_eq!(serde_json::to_string(&PropertyMap::new()).unwrap(), "{}");
    }
}

/// The `Shaped` representation (a map stored in a graph) behaves exactly like the `Indexed` one
/// through the public API: equality across representations, `Debug` text, iterators, serde, and
/// the conversion to `Indexed` on a key-changing mutation outside the graph's shape table.
#[cfg(test)]
mod shaped_tests {
    use super::{PropertyMap, Repr};
    use crate::graph::shape::ShapeTable;
    use crate::value::ElementProperty;

    fn sample() -> PropertyMap {
        PropertyMap::from([
            ("name", ElementProperty::from("marko")),
            ("age", ElementProperty::from(29)),
            ("ok", ElementProperty::from(true)),
        ])
    }

    fn shaped(table: &mut ShapeTable, map: &PropertyMap) -> PropertyMap {
        let mut copy = map.clone();
        table.adopt(&mut copy);
        assert!(matches!(copy.0, Repr::Shaped(_)));
        copy
    }

    fn is_indexed(map: &PropertyMap) -> bool {
        matches!(map.0, Repr::Indexed(_))
    }

    fn keys(map: &PropertyMap) -> Vec<&str> {
        map.keys().map(|k| k.as_str()).collect()
    }

    #[test]
    fn equality_holds_across_representations_and_ignores_order() {
        let mut table = ShapeTable::new();
        let indexed = sample();
        let shaped_map = shaped(&mut table, &indexed);
        assert_eq!(indexed, shaped_map);
        assert_eq!(shaped_map, indexed);
        assert_eq!(shaped_map, shaped_map.clone());
        let reversed: PropertyMap = indexed
            .iter()
            .rev()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let shaped_reversed = shaped(&mut table, &reversed);
        assert_eq!(shaped_map, shaped_reversed);
        assert_eq!(shaped_reversed, indexed);
        let mut other_value = indexed.clone();
        other_value.insert("age", ElementProperty::from(30));
        assert_ne!(shaped_map, other_value);
        assert_ne!(shaped(&mut table, &other_value), shaped_map);
        let mut longer = indexed.clone();
        longer.insert("extra", ElementProperty::Null);
        assert_ne!(shaped_map, longer);
        assert_ne!(longer, shaped_map);
        assert_eq!(shaped(&mut table, &PropertyMap::new()), PropertyMap::new());
        // Nested inside an object value.
        assert_eq!(
            ElementProperty::Object(Box::new(shaped_map.clone())),
            ElementProperty::Object(Box::new(reversed))
        );
    }

    #[test]
    fn debug_text_is_identical() {
        let mut table = ShapeTable::new();
        let indexed = sample();
        let mut shaped_map = shaped(&mut table, &indexed);
        assert_eq!(format!("{shaped_map:?}"), format!("{indexed:?}"));
        assert_eq!(format!("{shaped_map:#?}"), format!("{indexed:#?}"));
        assert_eq!(
            format!("{:?}", shaped_map.iter()),
            format!("{:?}", indexed.iter())
        );
        assert_eq!(
            format!("{:?}", shaped_map.keys()),
            format!("{:?}", indexed.keys())
        );
        assert_eq!(
            format!("{:?}", shaped_map.values()),
            format!("{:?}", indexed.values())
        );
        assert_eq!(
            format!("{:?}", shaped_map.iter_mut()),
            "IterMut { remaining: 3 }"
        );
        assert_eq!(
            format!("{:?}", shaped_map.values_mut()),
            "ValuesMut { remaining: 3 }"
        );
        assert_eq!(
            format!("{:?}", shaped_map.into_iter()),
            "IntoIter { remaining: 3 }"
        );
        assert_eq!(
            format!("{:?}", shaped(&mut table, &PropertyMap::new())),
            "{}"
        );
    }

    #[test]
    fn iterators_are_ordered_double_ended_and_exact() {
        let mut table = ShapeTable::new();
        let mut map = shaped(&mut table, &sample());
        assert_eq!(keys(&map), ["name", "age", "ok"]);
        assert_eq!(map.iter().len(), 3);
        assert_eq!(
            map.keys().rev().map(|k| k.as_str()).collect::<Vec<_>>(),
            ["ok", "age", "name"]
        );
        assert_eq!(map.values().next_back(), Some(&ElementProperty::from(true)));
        let mut iter = map.iter();
        assert_eq!(iter.next().map(|(k, _)| k.as_str()), Some("name"));
        assert_eq!(iter.next_back().map(|(k, _)| k.as_str()), Some("ok"));
        assert_eq!(iter.len(), 1);
        assert_eq!(iter.next().map(|(k, _)| k.as_str()), Some("age"));
        assert!(iter.next().is_none());
        assert!(iter.next().is_none(), "fused");
        assert_eq!(map.iter_mut().len(), 3);
        assert_eq!(map.values_mut().rev().count(), 3);
        let mut owned = map.clone().into_iter();
        assert_eq!(owned.len(), 3);
        assert_eq!(
            owned.next_back().map(|(k, _)| k.to_string()),
            Some("ok".into())
        );
        assert_eq!(
            owned.map(|(k, _)| k.to_string()).collect::<Vec<_>>(),
            ["name", "age"]
        );
        assert_eq!(map["age"], ElementProperty::from(29));
        assert!(map.contains_key("ok"));
        assert!(!map.contains_key("absent"));
        assert!(map.get("absent").is_none());
    }

    #[test]
    #[should_panic]
    fn indexing_an_absent_key_panics() {
        let mut table = ShapeTable::new();
        let _ = &shaped(&mut table, &sample())["absent"];
    }

    #[test]
    fn value_mutations_keep_the_shape() {
        let mut table = ShapeTable::new();
        let mut map = shaped(&mut table, &sample());
        *map.get_mut("age").unwrap() = ElementProperty::from(30);
        for (key, value) in map.iter_mut() {
            if key == "ok" {
                *value = ElementProperty::from(false);
            }
        }
        for value in map.values_mut() {
            if *value == ElementProperty::from("marko") {
                *value = ElementProperty::from("vadas");
            }
        }
        // Overwriting an existing key and removing an absent one change no key.
        assert_eq!(
            map.insert("name", ElementProperty::from("josh")),
            Some(ElementProperty::from("vadas"))
        );
        assert_eq!(map.shift_remove("absent"), None);
        assert!(!is_indexed(&map));
        assert_eq!(keys(&map), ["name", "age", "ok"]);
        assert_eq!(map["age"], ElementProperty::from(30));
        assert_eq!(map["ok"], ElementProperty::from(false));
    }

    #[test]
    fn key_changing_mutations_outside_the_graph_convert_to_indexed() {
        let mut table = ShapeTable::new();
        let base = shaped(&mut table, &sample());

        let mut added = base.clone();
        assert_eq!(added.insert("city", ElementProperty::from("rome")), None);
        assert!(is_indexed(&added));
        assert_eq!(keys(&added), ["name", "age", "ok", "city"]);

        let mut removed = base.clone();
        assert_eq!(removed.shift_remove("age"), Some(ElementProperty::from(29)));
        assert!(is_indexed(&removed));
        assert_eq!(keys(&removed), ["name", "ok"]);

        let mut extended = base.clone();
        extended.extend([("z", ElementProperty::Null)]);
        assert!(is_indexed(&extended));
        assert_eq!(keys(&extended), ["name", "age", "ok", "z"]);

        let mut cleared = base.clone();
        cleared.clear();
        assert!(cleared.is_empty());
        assert!(is_indexed(&cleared));
        cleared.insert("a", ElementProperty::Null);
        assert_eq!(keys(&cleared), ["a"]);

        // The original is untouched and still shares its shape.
        assert!(!is_indexed(&base));
        assert_eq!(base, sample());

        let mut target = PropertyMap::from([("zz", ElementProperty::Null)]);
        target.clone_from(&base);
        assert_eq!(keys(&target), ["name", "age", "ok"]);
        let mut shaped_target = base.clone();
        shaped_target.clone_from(&PropertyMap::from([("q", ElementProperty::Null)]));
        assert_eq!(keys(&shaped_target), ["q"]);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_text_is_identical() {
        let mut table = ShapeTable::new();
        let indexed = sample();
        let shaped_map = shaped(&mut table, &indexed);
        assert_eq!(
            serde_json::to_string(&shaped_map).unwrap(),
            serde_json::to_string(&indexed).unwrap()
        );
        let back: PropertyMap =
            serde_json::from_str(&serde_json::to_string(&shaped_map).unwrap()).unwrap();
        assert!(is_indexed(&back));
        assert_eq!(back, shaped_map);
    }

    #[test]
    fn heap_estimate_of_a_shaped_map_is_its_value_slice() {
        let mut table = ShapeTable::new();
        let map = shaped(&mut table, &sample());
        assert_eq!(
            crate::value::estimated_element_heap_bytes(&map),
            3 * size_of::<ElementProperty>() as u64
        );
        let long = "a string far longer than the inline limit";
        let with_heap = shaped(
            &mut table,
            &PropertyMap::from([("text", ElementProperty::from(long))]),
        );
        assert_eq!(
            crate::value::estimated_element_heap_bytes(&with_heap),
            size_of::<ElementProperty>() as u64 + long.len() as u64
        );
    }
}

/// Key prints: exact for keys of up to 16 bytes, confirmed by bytes beyond; lookups through them
/// find every key of a shape and nothing else.
#[cfg(test)]
mod key_print_tests {
    use super::{KeyPrint, Shape};
    use compact_str::CompactString;

    fn keys() -> Vec<String> {
        let mut keys: Vec<String> = vec![String::new()];
        for len in 1..=40 {
            keys.push("a".repeat(len));
            keys.push(format!("{}b", "a".repeat(len - 1)));
            keys.push(format!("b{}", "a".repeat(len - 1)));
        }
        // Long keys that share their first and last eight bytes.
        keys.push("prefix__MIDDLE_ONE__suffix".into());
        keys.push("prefix__MIDDLE_TWO__suffix".into());
        keys.push("ü".into());
        keys.push("üü".into());
        keys.sort();
        keys.dedup();
        keys
    }

    #[test]
    fn prints_are_exact_up_to_sixteen_bytes() {
        let keys = keys();
        for a in &keys {
            for b in &keys {
                let same = KeyPrint::of(a) == KeyPrint::of(b);
                if a.len() <= 16 && b.len() <= 16 {
                    assert_eq!(same, a == b, "{a:?} vs {b:?}");
                } else if a == b {
                    assert!(same);
                }
            }
        }
    }

    #[test]
    fn a_shape_finds_each_of_its_keys() {
        let keys = keys();
        for chunk in keys.chunks(7).chain(std::iter::once(&keys[..])) {
            let shape = Shape::new(chunk.iter().map(CompactString::from).collect(), 0);
            for (position, key) in chunk.iter().enumerate() {
                assert_eq!(shape.position(key), Some(position), "{key:?}");
            }
            for key in &keys {
                if !chunk.contains(key) {
                    assert_eq!(shape.position(key), None, "{key:?}");
                }
            }
            assert!(shape.has_keys(chunk.iter().map(String::as_str)));
            assert!(!shape.has_keys(chunk.iter().rev().map(String::as_str)) || chunk.len() < 2);
        }
    }
}
