//! The value dictionary of a top-level `dedup` compression rule ([`ValueDictionary`]): every
//! distinct value at the rule's key once, an element's row holds its `u32` code instead of the
//! 32-byte value slot (book page "Compressed Properties", section "Deduplicating repeated
//! values"; `value/property_map.rs`, the `Coded` representation).
//!
//! Safe Rust only (user decision): the entries live in append-only segments of `OnceLock`
//! slots. A segment is allocated once with a fixed capacity and never moves, a slot is set once
//! and never changes, so a reader gets `&ElementProperty` from a code with two acquire loads and
//! no lock, while a writer appends under the dictionary's mutex. Entries are never removed or
//! reused in place: the graph rebuilds a dictionary that holds too many unused entries (a new
//! dictionary, the rows re-coded; the old one lives on while an undo entry holds it).

use std::sync::{Arc, OnceLock};

use super::ElementProperty;
use super::identity::{identity_eq, identity_hash_u64};

/// The capacity of segment 0; segment `k` holds `BASE << k` entries.
const BASE: u64 = 8;
/// Enough segments for every `u32` code (`BASE << SEGMENTS` > 2^32).
const SEGMENTS: usize = 30;

/// One segment: fixed capacity, slots set once.
type Segment = Box<[OnceLock<ElementProperty>]>;

/// The interning side of a dictionary (behind its mutex).
#[derive(Default)]
struct Index {
    /// Code by identity hash.
    table: hashbrown::HashTable<u32>,
    /// The number of entries (the next code).
    len: u32,
    /// Entries added since the dictionary was built or last marked rebuilt.
    appended: usize,
    /// The entry count when the dictionary was last marked rebuilt.
    live_at_rebuild: usize,
}

/// The distinct values of one top-level `dedup` rule (module docs).
pub(crate) struct ValueDictionary {
    segments: Box<[OnceLock<Segment>]>,
    /// A `std` mutex (poisoning recovered): the map types keep their unwind-safety auto
    /// traits.
    index: std::sync::Mutex<Index>,
    /// Strings shorter than this many bytes are not coded (the rule's `min_bytes`).
    min_bytes: usize,
}

impl std::fmt::Debug for ValueDictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueDictionary")
            .field("len", &self.len())
            .field("min_bytes", &self.min_bytes)
            .finish()
    }
}

/// The segment and the offset of `code`.
#[inline]
fn locate(code: u32) -> (usize, usize) {
    let n = code as u64 + BASE;
    let k = (63 - n.leading_zeros()) as usize - BASE.trailing_zeros() as usize;
    (k, (n - (BASE << k)) as usize)
}

/// The bytes one entry costs in a dictionary beyond its value's heap: its slot and its slot in
/// the hash index. The stats and the estimate count every distinct value as
/// `DICTIONARY_ENTRY_BYTES + heap`.
pub(crate) const DICTIONARY_ENTRY_BYTES: u64 =
    (std::mem::size_of::<OnceLock<ElementProperty>>() + std::mem::size_of::<u32>()) as u64;

/// The bytes a coded value costs in its row (its code).
pub(crate) const CODE_BYTES: u64 = std::mem::size_of::<u32>() as u64;

impl ValueDictionary {
    /// An empty dictionary of a rule whose strings below `min_bytes` stay in the row.
    pub(crate) fn new(min_bytes: usize) -> Self {
        Self {
            segments: (0..SEGMENTS).map(|_| OnceLock::new()).collect(),
            index: std::sync::Mutex::new(Index::default()),
            min_bytes,
        }
    }

    /// The interning side (a poisoned lock is taken over: every update leaves it consistent).
    fn lock(&self) -> std::sync::MutexGuard<'_, Index> {
        self.index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The rule's `min_bytes`.
    pub(crate) fn min_bytes(&self) -> usize {
        self.min_bytes
    }

    /// Whether `value` is coded under this dictionary's rule: every logical value except a
    /// string shorter than `min_bytes`.
    #[inline]
    pub(crate) fn accepts(&self, value: &ElementProperty) -> bool {
        match value {
            ElementProperty::String(text) => text.len() >= self.min_bytes,
            ElementProperty::Compressed(_) | ElementProperty::Deduped(_) => false,
            other => !other.holds_compressed(),
        }
    }

    /// The value of `code` (lock-free).
    #[inline]
    pub(crate) fn get(&self, code: u32) -> Option<&ElementProperty> {
        let (segment, offset) = locate(code);
        self.segments.get(segment)?.get()?.get(offset)?.get()
    }

    /// The code of `value`, adding it when new; `Err(value)` when the dictionary is full (2^32
    /// entries): the value then stays in the row.
    pub(crate) fn intern(&self, value: ElementProperty) -> Result<u32, ElementProperty> {
        self.intern_reporting(value).map(|(code, _)| code)
    }

    /// [`Self::intern`] that also says whether the value was added (`true`) or found.
    pub(crate) fn intern_reporting(
        &self,
        value: ElementProperty,
    ) -> Result<(u32, bool), ElementProperty> {
        let hash = identity_hash_u64(&value);
        let mut index = self.lock();
        if let Some(code) = index.table.find(hash, |code| {
            self.get(*code)
                .is_some_and(|stored| identity_eq(stored, &value))
        }) {
            return Ok((*code, false));
        }
        let code = index.len;
        let Some(next) = code.checked_add(1) else {
            return Err(value);
        };
        let (segment, offset) = locate(code);
        let Some(segment_cell) = self.segments.get(segment) else {
            return Err(value);
        };
        let slots =
            segment_cell.get_or_init(|| (0..(BASE << segment)).map(|_| OnceLock::new()).collect());
        let Some(slot) = slots.get(offset) else {
            return Err(value);
        };
        slot.set(value)?;
        index.table.insert_unique(hash, code, |code| {
            self.get(*code).map_or(0, identity_hash_u64)
        });
        index.len = next;
        index.appended += 1;
        Ok((code, true))
    }

    /// The number of entries (live or not).
    pub(crate) fn len(&self) -> usize {
        self.lock().len as usize
    }

    /// `(entries added since the last rebuild, entries at the last rebuild)`.
    pub(crate) fn growth(&self) -> (usize, usize) {
        let index = self.lock();
        (index.appended, index.live_at_rebuild)
    }

    /// Marks the current entries as the rebuilt state (after a rebuild or a load pass).
    pub(crate) fn mark_rebuilt(&self) {
        let mut index = self.lock();
        index.appended = 0;
        index.live_at_rebuild = index.len as usize;
    }

    /// Every entry with its code.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (u32, &ElementProperty)> {
        let len = self.lock().len;
        (0..len).filter_map(|code| Some((code, self.get(code)?)))
    }

    /// The heap bytes: the allocated segments, every entry's own heap, the hash index and the
    /// segment array.
    pub(crate) fn heap_bytes(&self) -> u64 {
        let slot = std::mem::size_of::<OnceLock<ElementProperty>>() as u64;
        let segments: u64 = self
            .segments
            .iter()
            .filter_map(OnceLock::get)
            .map(|segment| segment.len() as u64 * slot)
            .sum();
        let heap: u64 = self
            .entries()
            .map(|(_, value)| value.estimated_heap_bytes())
            .sum();
        let table = super::property_map::hash_table_heap_bytes(
            self.lock().table.capacity(),
            std::mem::size_of::<u32>(),
        );
        let array = (self.segments.len() * std::mem::size_of::<OnceLock<Segment>>()) as u64;
        segments + heap + table + array
    }
}

/// A coded value outside its row: its dictionary and its code (the undo log keeps a replaced
/// coded value so; the dictionary lives as long as the reference).
#[derive(Clone, Debug)]
pub(crate) struct DictRef {
    pub(crate) dictionary: Arc<ValueDictionary>,
    pub(crate) code: u32,
}

impl DictRef {
    /// The value.
    pub(crate) fn get(&self) -> Option<&ElementProperty> {
        self.dictionary.get(self.code)
    }

    /// The value, cloned (`Null` for a code the dictionary does not hold: never expected).
    pub(crate) fn value(&self) -> ElementProperty {
        self.get().cloned().unwrap_or(ElementProperty::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_locate_their_segments() {
        assert_eq!(locate(0), (0, 0));
        assert_eq!(locate(7), (0, 7));
        assert_eq!(locate(8), (1, 0));
        assert_eq!(locate(23), (1, 15));
        assert_eq!(locate(24), (2, 0));
        assert!(locate(u32::MAX).0 < SEGMENTS);
    }

    #[test]
    fn interning_shares_identical_values_and_reads_them_back() {
        let dictionary = ValueDictionary::new(0);
        let mut codes = Vec::new();
        for i in 0..1000i64 {
            codes.push(dictionary.intern(ElementProperty::from(i % 100)).unwrap());
        }
        assert_eq!(dictionary.len(), 100);
        for (i, code) in codes.iter().enumerate() {
            assert_eq!(
                dictionary.get(*code),
                Some(&ElementProperty::from(i as i64 % 100))
            );
        }
        // 1 and 1.0 are two entries.
        let float = dictionary.intern(ElementProperty::from(1.0f64)).unwrap();
        assert_ne!(Ok(float), dictionary.intern(ElementProperty::from(1i64)));
        // References stay valid while the dictionary grows.
        let first = dictionary.get(0).unwrap();
        for i in 0..10_000i64 {
            let _ = dictionary.intern(ElementProperty::from(1_000_000 + i));
        }
        assert_eq!(first, &ElementProperty::from(0i64));
        assert!(dictionary.heap_bytes() > 10_000 * 32);
    }

    #[test]
    fn short_strings_below_min_bytes_are_not_accepted() {
        let dictionary = ValueDictionary::new(5);
        assert!(!dictionary.accepts(&ElementProperty::from("abc")));
        assert!(dictionary.accepts(&ElementProperty::from("abcdef")));
        assert!(dictionary.accepts(&ElementProperty::from(3i64)));
    }
}
