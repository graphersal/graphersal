//! The shape table of a [`crate::TraversalGraph`]: interned property key lists ("shapes", like
//! the hidden classes of a JavaScript engine) shared by the stored property maps.
//!
//! Every property map the graph stores is `Shaped` (`value/property_map.rs`): an `Arc` of an
//! interned [`Shape`] (the ordered key list) plus the element's own values in key order, so the
//! key names and the hash index of a map are paid once per distinct key sequence, not once per
//! element. Insertion order stays externally visible (`valueMap()`, `elementMap()`,
//! `properties()`) and is kept by the shape.
//!
//! - **Interning:** one shape per distinct key sequence ([`ShapeTable::intern`]); the shapes of
//!   this table carry its id (`Shape::owner`), so "is this map's shape one of mine" is one integer
//!   comparison.
//! - **Transitions:** a key-adding write moves a map to the next shape through a cache
//!   `(shape, position, key) -> shape` (an added key goes to the end; the undo log puts a removed
//!   key back at its position), a key-removing write through `(shape, position) -> shape`.
//!   Interning looks a key sequence up without allocating, the last shape first.
//! - **Choke points:** `insert_vertex_raw` / `insert_edge_raw` adopt every map that enters the
//!   graph (loads, replay, undo restores, repairs, merges, imports, `add_*`); every in-graph
//!   key-changing write goes through [`ShapeTable::insert_full`], [`ShapeTable::shift_remove_full`]
//!   or [`ShapeTable::shift_insert`], and a write through a generic map helper (jpath writes) is
//!   followed by [`ShapeTable::adopt`]. A map that missed adoption is still correct (`Indexed`),
//!   it only does not share its keys.
//! - **Sweep:** a shape no map references any more (only the table holds it) is removed by
//!   [`ShapeTable::sweep`]: when the table has doubled since the last sweep (checked when a new
//!   shape is interned), on `shrink_to_fit` and after a load. A shape referenced from anywhere (a
//!   stored map, an element image in the undo log, a clone in a result) is never removed, so a
//!   shape with this table's id is always in the table. Reads never touch the table.
//!
//! Design and invariants: the book page "In-Memory Storage Internals"
//! (`book/src/for_developers/storage_internals.md`).

use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use compact_str::CompactString;
use hashbrown::HashTable;

use crate::value::property_map::{KeyPrint, Repr, Shape, Shaped, hash_table_heap_bytes, keys_hash};
use crate::value::{ElementProperty, PropertyMap};

/// A cheap hasher for the transition caches, whose keys are a shape address, a position and
/// (for an insertion) a short key: one multiply per word instead of FNV's one per byte.
#[derive(Default)]
struct MixHasher(u64);

impl MixHasher {
    #[inline]
    fn mix(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl std::hash::Hasher for MixHasher {
    #[inline]
    fn finish(&self) -> u64 {
        // Fold the well-mixed high half into the low bits hashbrown picks buckets with.
        self.0 ^ (self.0 >> 29)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            for (slot, byte) in word.iter_mut().zip(chunk) {
                *slot = *byte;
            }
            self.mix(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.mix(u64::from(n));
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.mix(n);
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.mix(n as u64);
    }
}

type Mix = BuildHasherDefault<MixHasher>;

/// The last insertion transition: `(shape address, position, key print, key, next shape)`.
type InsertMemo = (usize, usize, KeyPrint, CompactString, Arc<Shape>);

/// The smallest shape count at which the table sweeps when it has doubled.
const MIN_SWEEP_AT: usize = 64;

/// The next shape table id (ids are never reused within a process).
static NEXT_TABLE_ID: AtomicU64 = AtomicU64::new(1);

/// The address of a shape, the key of the transition caches. Valid while the shape is in the
/// table (the table holds it), and the caches are cleared before a sweep removes any shape.
#[inline]
fn address(shape: &Arc<Shape>) -> usize {
    Arc::as_ptr(shape) as usize
}

/// The interned shapes of one graph and their transition caches (see the module docs).
pub(crate) struct ShapeTable {
    /// This table's id, stamped on its shapes.
    id: u64,
    /// The interned non-empty shapes, hashed by their key sequence ([`keys_hash`]).
    shapes: HashTable<Arc<Shape>>,
    /// `(shape, position, inserted key) -> next shape` (an added key is inserted at the end).
    inserted: HashMap<(usize, usize, CompactString), Arc<Shape>, Mix>,
    /// `(shape, removed position) -> next shape`.
    removed: HashMap<(usize, usize), Arc<Shape>, Mix>,
    /// The last insertion transition, tried before `inserted` (a bulk write repeats one).
    last_insert: Option<InsertMemo>,
    /// The last removal transition `(shape, position, next)`, tried before `removed`.
    last_remove: Option<(usize, usize, Arc<Shape>)>,
    /// The last foreign shape adopted and this table's shape for it (a decoded chunk hands in
    /// one foreign shape for many elements).
    last_foreign: Option<(Arc<Shape>, Arc<Shape>)>,
    /// The empty shape: always this table's, never in `shapes` (so a graph whose elements have
    /// no properties reports no shape bytes) and never swept.
    empty: Arc<Shape>,
    /// The shape interned or adopted last: loads and bulk inserts repeat one key sequence, so
    /// [`Self::adopt`] compares against it before hashing the keys.
    last: Option<Arc<Shape>>,
    /// The shape count that triggers the next sweep.
    sweep_at: usize,
}

impl Default for ShapeTable {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ShapeTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShapeTable")
            .field("id", &self.id)
            .field("shapes", &self.shapes.len())
            .finish()
    }
}

impl ShapeTable {
    /// An empty table (holding only the empty shape, outside the set).
    pub(crate) fn new() -> Self {
        let id = NEXT_TABLE_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            id,
            shapes: HashTable::new(),
            inserted: HashMap::default(),
            removed: HashMap::default(),
            last_insert: None,
            last_remove: None,
            last_foreign: None,
            empty: Arc::new(Shape::new(Box::new([]), id)),
            last: None,
            sweep_at: MIN_SWEEP_AT,
        }
    }

    /// The number of interned non-empty shapes.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.shapes.len()
    }

    /// Whether `shape` is one of this table's.
    #[inline]
    fn owns(&self, shape: &Shape) -> bool {
        shape.owner == self.id
    }

    /// The interned shape of `keys` (unique, in order), interning it when new (tests).
    #[cfg(test)]
    pub(crate) fn intern(&mut self, keys: &[CompactString]) -> Arc<Shape> {
        self.intern_keys(keys.iter().map(CompactString::as_str), || keys.into())
    }

    /// The interned shape of the key sequence `keys`; `build` makes the owned key list only when
    /// the sequence is new (a found shape costs no allocation). The last shape is tried first,
    /// then the hash table.
    fn intern_keys<'a, I>(
        &mut self,
        keys: I,
        build: impl FnOnce() -> Box<[CompactString]>,
    ) -> Arc<Shape>
    where
        I: ExactSizeIterator<Item = &'a str> + Clone,
    {
        if keys.len() == 0 {
            return Arc::clone(&self.empty);
        }
        if let Some(last) = &self.last
            && last.has_keys(keys.clone())
        {
            return Arc::clone(last);
        }
        let hash = keys_hash(keys.clone());
        let found = self
            .shapes
            .find(hash, |shape| {
                shape.hash == hash && shape.has_keys(keys.clone())
            })
            .map(Arc::clone);
        let shape = match found {
            Some(shape) => shape,
            None => self.intern_new(build()),
        };
        self.last = Some(Arc::clone(&shape));
        shape
    }

    /// Interns a key list known to be absent, sweeping first when the table has doubled.
    fn intern_new(&mut self, keys: Box<[CompactString]>) -> Arc<Shape> {
        if self.shapes.len() >= self.sweep_at {
            self.sweep();
        }
        let shape = Arc::new(Shape::new(keys, self.id));
        self.shapes
            .insert_unique(shape.hash, Arc::clone(&shape), |shape| shape.hash);
        shape
    }

    /// Makes `map` a `Shaped` map of this table: an `Indexed` map is converted (its keys
    /// interned, its values moved into an exact vector), a `Shaped` map of another table (a map
    /// moved between graphs) gets this table's shape of the same keys. A map of this table is
    /// left alone (one integer comparison).
    #[inline]
    pub(crate) fn adopt(&mut self, map: &mut PropertyMap) {
        if let Repr::Shaped(shaped) = &map.0
            && self.owns(&shaped.shape)
        {
            return;
        }
        self.adopt_slow(map);
    }

    /// [`Self::adopt`] of a map that is not one of this table's yet.
    fn adopt_slow(&mut self, map: &mut PropertyMap) {
        match &mut map.0 {
            Repr::Shaped(shaped) => {
                if let Some((theirs, ours)) = &self.last_foreign
                    && Arc::ptr_eq(theirs, &shaped.shape)
                {
                    shaped.shape = Arc::clone(ours);
                    return;
                }
                let theirs = Arc::clone(&shaped.shape);
                shaped.shape = self
                    .intern_keys(theirs.keys.iter().map(CompactString::as_str), || {
                        theirs.keys.clone()
                    });
                // Only a foreign shape is remembered: the memo never names one of ours.
                if !self.owns(&theirs) {
                    self.last_foreign = Some((theirs, Arc::clone(&shaped.shape)));
                }
            }
            Repr::Indexed(indexed) => {
                let shape = self.intern_keys(indexed.keys().map(CompactString::as_str), || {
                    indexed.keys().cloned().collect()
                });
                // Exactly sized: a plain `collect` would reuse the entry allocation in place (twice the
                // size of the values).
                let indexed = std::mem::take(indexed);
                let mut values = Vec::with_capacity(indexed.len());
                values.extend(indexed.into_values());
                map.0 = Repr::Shaped(Shaped { shape, values });
            }
        }
    }

    /// [`Self::adopt`] of an edge's optional map.
    #[inline]
    pub(crate) fn adopt_optional(&mut self, map: &mut Option<PropertyMap>) {
        if let Some(map) = map {
            self.adopt(map);
        }
    }

    /// The in-graph `insert`: `(position, previous value)`. An existing key is overwritten in
    /// place; a new key moves the map to the next shape (transition cache) and grows its values
    /// by one slot (no reallocation when a removed key left one).
    #[inline]
    pub(crate) fn insert_full(
        &mut self,
        map: &mut PropertyMap,
        key: CompactString,
        value: ElementProperty,
    ) -> (usize, Option<ElementProperty>) {
        self.adopt(map);
        let Repr::Shaped(shaped) = &mut map.0 else {
            return map.insert_full(key, value);
        };
        if let Some(position) = shaped.shape.position(&key)
            && let Some(slot) = shaped.values.get_mut(position)
        {
            return (position, Some(std::mem::replace(slot, value)));
        }
        let position = shaped.values.len();
        shaped.shape = self.with_key_at(&shaped.shape, position, key);
        shaped.values.reserve_exact(1);
        shaped.values.push(value);
        (position, None)
    }

    /// The in-graph `shift_remove`: `(position, key, value)` of the removed entry; the map moves
    /// to the shape without that position (transition cache). The value slot stays as capacity.
    /// An absent key changes nothing.
    pub(crate) fn shift_remove_full(
        &mut self,
        map: &mut PropertyMap,
        key: &str,
    ) -> Option<(usize, CompactString, ElementProperty)> {
        self.adopt(map);
        let Repr::Shaped(shaped) = &mut map.0 else {
            return map.shift_remove_full(key);
        };
        let position = shaped.shape.position(key)?;
        if position >= shaped.values.len() {
            return None;
        }
        let stored = shaped.shape.keys.get(position)?.clone();
        shaped.shape = self.without_position(&shaped.shape, position);
        let value = shaped.values.remove(position);
        Some((position, stored, value))
    }

    /// The in-graph positional insert of an absent `key` at `position` (clamped to the length):
    /// the undo log putting a removed key back where it was (transition cache). A present key is
    /// overwritten in place.
    pub(crate) fn shift_insert(
        &mut self,
        map: &mut PropertyMap,
        position: usize,
        key: CompactString,
        value: ElementProperty,
    ) {
        self.adopt(map);
        let Repr::Shaped(shaped) = &mut map.0 else {
            map.shift_insert(position, key, value);
            return;
        };
        if let Some(existing) = shaped.shape.position(&key)
            && let Some(slot) = shaped.values.get_mut(existing)
        {
            *slot = value;
            return;
        }
        let position = position.min(shaped.values.len());
        shaped.shape = self.with_key_at(&shaped.shape, position, key);
        shaped.values.reserve_exact(1);
        shaped.values.insert(position, value);
    }

    /// The shape of `shape`'s keys with `key` inserted at `position` (`shape` is one of this
    /// table's, `position <= shape.len()`).
    fn with_key_at(
        &mut self,
        shape: &Arc<Shape>,
        position: usize,
        key: CompactString,
    ) -> Arc<Shape> {
        let from = address(shape);
        let print = KeyPrint::of(&key);
        if let Some((at, memo_position, memo_print, memo_key, next)) = &self.last_insert
            && *at == from
            && *memo_position == position
            && *memo_print == print
            && print.confirms(memo_key, &key)
        {
            return Arc::clone(next);
        }
        let probe = (from, position, key);
        if let Some(next) = self.inserted.get(&probe) {
            let next = Arc::clone(next);
            let (_, _, key) = probe;
            self.last_insert = Some((from, position, print, key, Arc::clone(&next)));
            return next;
        }
        let (_, _, key) = probe;
        let (before, after) = shape.keys.split_at(position.min(shape.len()));
        let keys = before
            .iter()
            .map(CompactString::as_str)
            .chain(std::iter::once(key.as_str()))
            .chain(after.iter().map(CompactString::as_str));
        let next = self.intern_keys(ExactChain(keys, shape.len() + 1), || {
            before
                .iter()
                .cloned()
                .chain(std::iter::once(key.clone()))
                .chain(after.iter().cloned())
                .collect()
        });
        // A sweep inside `intern_keys` clears the caches but never removes `shape` (the map
        // being written holds it), so `from` still names a shape of the table.
        self.last_insert = Some((from, position, print, key.clone(), Arc::clone(&next)));
        self.inserted
            .insert((from, position, key), Arc::clone(&next));
        next
    }

    /// The shape of `shape`'s keys without the one at `position`.
    fn without_position(&mut self, shape: &Arc<Shape>, position: usize) -> Arc<Shape> {
        let probe = (address(shape), position);
        if let Some((at, memo_position, next)) = &self.last_remove
            && *at == probe.0
            && *memo_position == position
        {
            return Arc::clone(next);
        }
        if let Some(next) = self.removed.get(&probe) {
            let next = Arc::clone(next);
            self.last_remove = Some((probe.0, position, Arc::clone(&next)));
            return next;
        }
        let keys = shape
            .keys
            .iter()
            .enumerate()
            .filter(move |(at, _)| *at != position)
            .map(|(_, key)| key.as_str());
        let len = shape.len().saturating_sub(1);
        let next = self.intern_keys(ExactChain(keys, len), || {
            shape
                .keys
                .iter()
                .enumerate()
                .filter(|(at, _)| *at != position)
                .map(|(_, key)| key.clone())
                .collect()
        });
        self.last_remove = Some((probe.0, position, Arc::clone(&next)));
        self.removed.insert(probe, Arc::clone(&next));
        next
    }

    /// Removes every shape no map references any more (only the table holds it) and clears the
    /// transition caches (they hold shapes and are keyed by address). Returns how many shapes
    /// were removed. The next threshold sweep comes when the table has doubled again.
    pub(crate) fn sweep(&mut self) -> usize {
        self.inserted.clear();
        self.removed.clear();
        self.last_insert = None;
        self.last_remove = None;
        self.last_foreign = None;
        self.last = None;
        let before = self.shapes.len();
        // Held by the table alone: one strong reference (the set's).
        self.shapes.retain(|shape| Arc::strong_count(shape) > 1);
        self.sweep_at = (self.shapes.len() * 2).max(MIN_SWEEP_AT);
        before - self.shapes.len()
    }

    /// Sweeps and releases the spare capacity of the set and the caches.
    pub(crate) fn shrink_to_fit(&mut self) {
        self.sweep();
        self.shapes.shrink_to_fit(|shape| shape.hash);
        self.inserted.shrink_to_fit();
        self.removed.shrink_to_fit();
    }

    /// The bytes of the table: every interned non-empty shape once ([`Shape::heap_bytes`]), the
    /// set's and the caches' hash tables (`MemoryStats::shape_bytes`). The empty shape, a
    /// constant of every graph, is not counted.
    pub(crate) fn heap_bytes(&self) -> u64 {
        use std::mem::size_of;
        let shapes: u64 = self.shapes.iter().map(|shape| shape.heap_bytes()).sum();
        let inserted_keys: u64 = self
            .inserted
            .keys()
            .map(|(_, _, key)| crate::value::compact_string_heap_bytes(key))
            .sum();
        shapes
            + hash_table_heap_bytes(self.shapes.capacity(), size_of::<Arc<Shape>>())
            + hash_table_heap_bytes(
                self.inserted.capacity(),
                size_of::<((usize, usize, CompactString), Arc<Shape>)>(),
            )
            + inserted_keys
            + hash_table_heap_bytes(
                self.removed.capacity(),
                size_of::<((usize, usize), Arc<Shape>)>(),
            )
    }
}

/// An iterator with a known exact length (the chained and filtered key sequences of a
/// transition, whose adapters do not implement `ExactSizeIterator` themselves).
#[derive(Clone)]
struct ExactChain<I>(I, usize);

impl<I: Iterator> Iterator for ExactChain<I> {
    type Item = I::Item;

    #[inline]
    fn next(&mut self) -> Option<I::Item> {
        let item = self.0.next()?;
        self.1 = self.1.saturating_sub(1);
        Some(item)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.1, Some(self.1))
    }
}

impl<I: Iterator> ExactSizeIterator for ExactChain<I> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<CompactString> {
        names.iter().map(|n| CompactString::from(*n)).collect()
    }

    #[test]
    fn the_same_key_sequence_gives_the_same_shape() {
        let mut table = ShapeTable::new();
        let a = table.intern(&keys(&["name", "age"]));
        let b = table.intern(&keys(&["name", "age"]));
        let c = table.intern(&keys(&["age", "name"]));
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &c));
        assert!(Arc::ptr_eq(&table.intern(&[]), &table.empty));
    }

    #[test]
    fn transitions_reach_the_interned_shape() {
        let mut table = ShapeTable::new();
        let mut first = PropertyMap::new();
        let mut second = PropertyMap::new();
        for map in [&mut first, &mut second] {
            table.adopt(map);
            table.insert_full(map, "name".into(), 1.into());
            table.insert_full(map, "age".into(), 2.into());
        }
        let direct = table.intern(&keys(&["name", "age"]));
        assert!(Arc::ptr_eq(first.shape().unwrap(), &direct));
        assert!(Arc::ptr_eq(second.shape().unwrap(), &direct));
        // Overwrite: same shape, value replaced, position reported.
        assert_eq!(
            table.insert_full(&mut first, "name".into(), 3.into()),
            (0, Some(1.into()))
        );
        assert!(Arc::ptr_eq(first.shape().unwrap(), &direct));
        // Removal and positional re-insert come back to the same shape.
        let (position, key, value) = table.shift_remove_full(&mut first, "name").unwrap();
        assert_eq!((position, key.as_str(), &value), (0, "name", &3.into()));
        assert!(Arc::ptr_eq(
            first.shape().unwrap(),
            &table.intern(&keys(&["age"]))
        ));
        assert!(table.shift_remove_full(&mut first, "absent").is_none());
        table.shift_insert(&mut first, position, key, value);
        assert!(Arc::ptr_eq(first.shape().unwrap(), &direct));
        assert_eq!(
            first.keys().map(|k| k.as_str()).collect::<Vec<_>>(),
            ["name", "age"]
        );
        assert_eq!(first["name"], 3.into());
    }

    #[test]
    fn sweep_frees_only_unreferenced_shapes() {
        let mut table = ShapeTable::new();
        let mut kept = PropertyMap::from([("a", ElementProperty::Null)]);
        table.adopt(&mut kept);
        let mut held = PropertyMap::from([("b", ElementProperty::Null)]);
        table.adopt(&mut held);
        // A clone outside the table (an undo image, a result) keeps its shape alive.
        let image = held.clone();
        drop(held);
        let mut dropped = PropertyMap::from([("c", ElementProperty::Null)]);
        table.adopt(&mut dropped);
        drop(dropped);
        // a, b and c (the empty shape is not in the set).
        assert_eq!(table.len(), 3);
        assert_eq!(table.sweep(), 1);
        assert_eq!(table.len(), 2);
        assert!(Arc::ptr_eq(
            image.shape().unwrap(),
            &table.intern(&keys(&["b"]))
        ));
        drop(image);
        assert_eq!(table.sweep(), 1);
        assert!(Arc::ptr_eq(
            kept.shape().unwrap(),
            &table.intern(&keys(&["a"]))
        ));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn a_map_of_another_table_is_reinterned() {
        let mut one = ShapeTable::new();
        let mut two = ShapeTable::new();
        let mut map = PropertyMap::from([("x", ElementProperty::from(1))]);
        one.adopt(&mut map);
        let theirs = Arc::clone(map.shape().unwrap());
        two.adopt(&mut map);
        assert!(!Arc::ptr_eq(map.shape().unwrap(), &theirs));
        assert!(Arc::ptr_eq(
            map.shape().unwrap(),
            &two.intern(&keys(&["x"]))
        ));
        assert_eq!(map["x"], 1.into());
    }

    #[test]
    fn the_table_sweeps_when_it_has_doubled() {
        let mut table = ShapeTable::new();
        for i in 0..(MIN_SWEEP_AT * 3) {
            let mut map = PropertyMap::from([(format!("k{i}"), ElementProperty::Null)]);
            table.adopt(&mut map);
        }
        // Nothing referenced the shapes: the table never grew past the threshold.
        assert!(table.len() <= MIN_SWEEP_AT, "{}", table.len());
    }

    #[test]
    fn wide_shapes_keep_an_index() {
        let mut table = ShapeTable::new();
        let names: Vec<String> = (0..20).map(|i| format!("key{i}")).collect();
        let mut map = PropertyMap::new();
        for (i, name) in names.iter().enumerate() {
            table.insert_full(&mut map, name.as_str().into(), (i as i64).into());
        }
        let shape = map.shape().unwrap();
        assert!(shape.is_wide());
        assert!(!table.intern(&keys(&["a"])).is_wide());
        for (i, name) in names.iter().enumerate() {
            assert_eq!(map.get(name), Some(&(i as i64).into()));
        }
        assert!(map.get("key20").is_none());
        let (position, _, _) = table.shift_remove_full(&mut map, "key3").unwrap();
        assert_eq!(position, 3);
        assert_eq!(map.get("key4"), Some(&4.into()));
        assert!(map.shape().unwrap().len() == 19);
    }

    #[test]
    fn heap_bytes_count_each_shape_once() {
        let mut table = ShapeTable::new();
        let empty = table.heap_bytes();
        let mut maps: Vec<PropertyMap> = (0..10)
            .map(|i| PropertyMap::from([("name", ElementProperty::from(i)), ("age", 1.into())]))
            .collect();
        for map in &mut maps {
            table.adopt(map);
        }
        let one_shape = table.heap_bytes() - empty;
        let shape_bytes = maps[0].shape().unwrap().heap_bytes();
        assert!(one_shape >= shape_bytes);
        assert!(
            one_shape < 2 * shape_bytes + 512,
            "{one_shape} vs {shape_bytes}"
        );
    }
}

/// The shape table inside a `TraversalGraph`: stored maps are shaped and share their key lists,
/// writes transition, rollbacks come back to the same shape, a sweep never frees a shape an open
/// unit's undo log still holds.
#[cfg(test)]
mod graph_tests {
    use std::sync::Arc;

    use compact_str::CompactString;

    use crate::TraversalGraph;
    use crate::graph::{GraphStorage, RollbackReason, VertexHandle};
    use crate::value::property_map::{Repr, Shape};
    use crate::value::{ElementProperty, PropertyMap};

    fn person(graph: &mut TraversalGraph, name: &str, age: i64) -> VertexHandle {
        let props = PropertyMap::from([
            ("name", ElementProperty::from(name)),
            ("age", ElementProperty::from(age)),
        ]);
        graph
            .insert_vertex(None, vec!["person".into()], props)
            .expect("insert")
    }

    fn shape_of(graph: &TraversalGraph, handle: VertexHandle) -> Arc<Shape> {
        let vertex = graph.vertex_weight(handle).expect("live");
        match &vertex.2.0 {
            Repr::Shaped(shaped) => Arc::clone(&shaped.shape),
            Repr::Indexed(_) => panic!("a stored map is shaped"),
        }
    }

    fn keys(graph: &TraversalGraph, handle: VertexHandle) -> Vec<String> {
        graph
            .vertex_properties(handle)
            .expect("live")
            .keys()
            .map(|k| k.to_string())
            .collect()
    }

    #[test]
    fn elements_with_the_same_keys_share_one_shape() {
        let mut graph = TraversalGraph::new();
        let a = person(&mut graph, "ann", 30);
        let b = person(&mut graph, "bob", 40);
        assert!(Arc::ptr_eq(&shape_of(&graph, a), &shape_of(&graph, b)));
        // Edges too, and an edge without properties stays `None`.
        let e1 = graph
            .insert_edge(
                None,
                Some("knows".into()),
                a,
                b,
                PropertyMap::from([("w", ElementProperty::from(1))]),
            )
            .expect("edge");
        let e2 = graph
            .insert_edge(
                None,
                Some("knows".into()),
                b,
                a,
                PropertyMap::from([("w", ElementProperty::from(2))]),
            )
            .expect("edge");
        let edge_shape = |handle| match &graph.edge_weight(handle).expect("live").2 {
            Some(PropertyMap(Repr::Shaped(shaped))) => Arc::clone(&shaped.shape),
            _ => panic!("a stored edge map is shaped"),
        };
        assert!(Arc::ptr_eq(&edge_shape(e1), &edge_shape(e2)));
    }

    #[test]
    fn writes_transition_and_rollback_returns_to_the_same_shape() {
        let mut graph = TraversalGraph::new();
        let a = person(&mut graph, "ann", 30);
        let b = person(&mut graph, "bob", 40);
        let before = shape_of(&graph, a);
        let mark = graph.begin_unit();
        graph
            .set_vertex_property(a, "city".into(), "rome".into())
            .expect("set");
        graph
            .set_vertex_property(b, "city".into(), "oslo".into())
            .expect("set");
        assert!(Arc::ptr_eq(&shape_of(&graph, a), &shape_of(&graph, b)));
        assert_eq!(keys(&graph, a), ["name", "age", "city"]);
        graph.drop_vertex_property("name", a).expect("drop");
        assert_eq!(keys(&graph, a), ["age", "city"]);
        graph.rollback_unit(mark, &RollbackReason::failed(&"test"));
        assert_eq!(keys(&graph, a), ["name", "age"]);
        assert!(Arc::ptr_eq(&shape_of(&graph, a), &before));
        assert!(Arc::ptr_eq(&shape_of(&graph, b), &before));
    }

    #[test]
    fn a_sweep_keeps_the_shapes_an_open_unit_holds() {
        let mut graph = TraversalGraph::new();
        let a = person(&mut graph, "ann", 30);
        graph
            .set_vertex_property(a, "only_here".into(), 1.into())
            .expect("set");
        let id = graph.vertex_weight(a).expect("live").0.clone();
        let mark = graph.begin_unit();
        graph.drop_vertex(a).expect("drop");
        // The element image in the undo log holds the shape: a sweep must keep it.
        graph.shrink_to_fit();
        let held = graph.shapes.len();
        graph.rollback_unit(mark, &RollbackReason::failed(&"test"));
        let restored = graph.index.get_node(&graph.graph, &id).expect("restored");
        assert_eq!(keys(&graph, restored), ["name", "age", "only_here"]);
        assert_eq!(graph.shapes.len(), held);
        // Once nothing holds it, the next sweep frees it (and its prefix shapes).
        graph.drop_vertex(restored).expect("drop");
        assert!(graph.shapes.sweep() >= 1);
        assert_eq!(graph.shapes.len(), 0, "only the empty shape is left");
        graph.shrink_to_fit();
        assert_eq!(graph.shapes.heap_bytes(), 0);
    }

    #[test]
    fn a_map_from_another_graph_gets_this_graphs_shape() {
        let mut one = TraversalGraph::new();
        let a = person(&mut one, "ann", 30);
        let copy = one.vertex_properties(a).expect("live").clone();
        let mut two = TraversalGraph::new();
        let b = two.insert_vertex(None, vec![], copy).expect("insert");
        let local = person(&mut two, "bob", 40);
        assert!(!Arc::ptr_eq(&shape_of(&one, a), &shape_of(&two, b)));
        assert!(Arc::ptr_eq(&shape_of(&two, b), &shape_of(&two, local)));
        assert_eq!(keys(&two, b), ["name", "age"]);
    }

    #[test]
    fn a_path_write_keeps_the_map_shaped() {
        let mut graph = TraversalGraph::new();
        let a = person(&mut graph, "ann", 30);
        let path = "address.city"
            .parse::<crate::value::JsonPath>()
            .expect("path");
        graph
            .set_vertex_property_path(a, &path, "rome".into())
            .expect("write");
        let b = person(&mut graph, "bob", 40);
        graph
            .set_vertex_property(
                b,
                CompactString::from("address"),
                ElementProperty::Object(Box::new(PropertyMap::from([(
                    "city",
                    ElementProperty::from("oslo"),
                )]))),
            )
            .expect("set");
        assert!(Arc::ptr_eq(&shape_of(&graph, a), &shape_of(&graph, b)));
        graph.remove_vertex_property_path(a, &path).expect("remove");
        assert!(Arc::ptr_eq(&shape_of(&graph, a), &shape_of(&graph, b)));
    }

    #[test]
    fn memory_usage_counts_each_shape_once() {
        let mut small = TraversalGraph::new();
        person(&mut small, "ann", 30);
        let mut large = TraversalGraph::new();
        for i in 0..1000 {
            person(&mut large, &format!("p{i}"), i);
        }
        let small_stats = small.memory_usage().expect("stats");
        let large_stats = large.memory_usage().expect("stats");
        assert_eq!(small_stats.shape_bytes, large_stats.shape_bytes);
        assert_eq!(small_stats.shape_bytes, small.shapes.heap_bytes());
        assert!(large_stats.property_bytes >= large_stats.shape_bytes);
        // Two short values per vertex: 2 x 32 bytes of slots each, no key bytes.
        let per_vertex = (large_stats.property_bytes - large_stats.shape_bytes) / 1000;
        assert_eq!(per_vertex, 2 * size_of::<ElementProperty>() as u64);
    }
}
