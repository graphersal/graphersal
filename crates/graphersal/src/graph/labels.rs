//! The label table of a [`TraversalGraph`](crate::TraversalGraph): every distinct edge label and
//! every distinct vertex label SET once, so an element slot holds a 4-byte number instead of
//! strings (a vertex's label list was a `Vec` of its own: 24 bytes in the slot plus 24 bytes of
//! heap per label; an edge's label 24 bytes).
//!
//! A vertex's labels are interned as a whole ordered set, so [`Labels::set`] returns the slice
//! [`GraphStorage::vertex_labels`](crate::graph::GraphStorage::vertex_labels) hands out under a
//! shared borrow. Entries are never removed: a graph has few distinct labels and label sets, and
//! [`Labels::heap_bytes`] reports what they cost.

use compact_str::CompactString;
use hashbrown::HashTable;

use crate::value::property_map::keys_hash;

/// The number of an interned vertex label set ([`Labels::set`]); `EMPTY` is the empty set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct LabelSetId(u32);

impl LabelSetId {
    /// The empty label set (an unlabeled vertex), always number 0.
    pub(crate) const EMPTY: LabelSetId = LabelSetId(0);
}

/// The number of an interned edge label ([`Labels::name`]), or `NONE` for an unlabeled edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct LabelId(u32);

impl LabelId {
    /// No label.
    pub(crate) const NONE: LabelId = LabelId(u32::MAX);
}

/// The interned edge labels and vertex label sets of one graph (module docs). Every string is
/// stored once, in `names` / `sets`; the hash tables hold only numbers into them.
#[derive(Debug, Clone)]
pub(crate) struct Labels {
    names: Vec<CompactString>,
    /// Numbers into `names`, hashed by the label.
    name_ids: HashTable<u32>,
    sets: Vec<Box<[CompactString]>>,
    /// Numbers into `sets` (the empty set 0 excluded), hashed by the ordered labels.
    set_ids: HashTable<u32>,
    /// The set interned last (loads and bulk inserts repeat one).
    last_set: LabelSetId,
    /// The edge label interned last.
    last_name: LabelId,
    /// The bytes of the label set contents (a slot and the heap of each label), kept as sets are
    /// added, so [`Self::heap_bytes`] needs no walk.
    set_content_bytes: u64,
}

impl Default for Labels {
    fn default() -> Self {
        Labels {
            names: Vec::new(),
            name_ids: HashTable::new(),
            sets: vec![Box::new([])],
            set_ids: HashTable::new(),
            last_set: LabelSetId::EMPTY,
            last_name: LabelId::NONE,
            set_content_bytes: 0,
        }
    }
}

/// The hash of an ordered label set.
fn set_hash(labels: &[CompactString]) -> u64 {
    keys_hash(labels.iter().map(CompactString::as_str))
}

/// The bytes of one label set's contents: a slot per label plus each label's heap.
fn set_bytes(labels: &[CompactString]) -> u64 {
    labels.len() as u64 * std::mem::size_of::<CompactString>() as u64
        + labels
            .iter()
            .map(crate::value::compact_string_heap_bytes)
            .sum::<u64>()
}

/// The hash of one label.
fn name_hash(label: &str) -> u64 {
    keys_hash(std::iter::once(label))
}

impl Labels {
    /// The number of the label set `labels` (ordered as given), interned on first use.
    pub(crate) fn intern_set(&mut self, labels: &[CompactString]) -> LabelSetId {
        if labels.is_empty() {
            return LabelSetId::EMPTY;
        }
        if self.set(self.last_set) == labels {
            return self.last_set;
        }
        let hash = set_hash(labels);
        let sets = &self.sets;
        let found = self.set_ids.find(hash, |&id| {
            sets.get(id as usize).is_some_and(|set| **set == *labels)
        });
        let id = match found {
            Some(&id) => LabelSetId(id),
            None => {
                let id = self.sets.len() as u32;
                self.set_content_bytes += set_bytes(labels);
                self.sets.push(labels.into());
                let sets = &self.sets;
                self.set_ids.insert_unique(hash, id, |&id| {
                    sets.get(id as usize).map_or(0, |set| set_hash(set))
                });
                LabelSetId(id)
            }
        };
        self.last_set = id;
        id
    }

    /// The labels of set `id` (empty for an unknown number).
    #[inline]
    pub(crate) fn set(&self, id: LabelSetId) -> &[CompactString] {
        self.sets.get(id.0 as usize).map_or(&[], |set| set)
    }

    /// The number of the edge label `label` (`NONE` for `None`), interned on first use.
    pub(crate) fn intern_name(&mut self, label: Option<&str>) -> LabelId {
        let Some(label) = label else {
            return LabelId::NONE;
        };
        if self.name_str(self.last_name) == Some(label) {
            return self.last_name;
        }
        let hash = name_hash(label);
        let names = &self.names;
        if let Some(&id) = self.name_ids.find(hash, |&id| {
            names.get(id as usize).is_some_and(|name| name == label)
        }) {
            self.last_name = LabelId(id);
            return LabelId(id);
        }
        let id = self.names.len() as u32;
        self.names.push(CompactString::from(label));
        let names = &self.names;
        self.name_ids.insert_unique(hash, id, |&id| {
            names.get(id as usize).map_or(0, |name| name_hash(name))
        });
        self.last_name = LabelId(id);
        LabelId(id)
    }

    /// The edge label `id` (`None` for `NONE`).
    #[inline]
    pub(crate) fn name(&self, id: LabelId) -> Option<&CompactString> {
        self.names.get(id.0 as usize)
    }

    /// The edge label `id` as text (`None` for `NONE`).
    #[inline]
    pub(crate) fn name_str(&self, id: LabelId) -> Option<&str> {
        self.name(id).map(CompactString::as_str)
    }

    /// The edge label `id` as a slice of zero or one label (the form the schema checks take).
    #[inline]
    pub(crate) fn name_slice(&self, id: LabelId) -> &[CompactString] {
        self.name(id).map_or(&[], std::slice::from_ref)
    }

    /// The heap bytes of the table: every label string and set once, with the vectors and hash
    /// maps that hold them.
    pub(crate) fn heap_bytes(&self) -> u64 {
        use crate::value::compact_string_heap_bytes as heap;
        use std::mem::size_of;
        let string = size_of::<CompactString>() as u64;
        // A hash table bucket: the number and one control byte, over its capacity.
        let names = self.names.capacity() as u64 * string
            + self.names.iter().map(heap).sum::<u64>()
            + self.name_ids.capacity() as u64 * (4 + 1);
        let set_slot = size_of::<Box<[CompactString]>>() as u64;
        let sets = self.sets.capacity() as u64 * set_slot
            + self.set_content_bytes
            + self.set_ids.capacity() as u64 * (4 + 1);
        names + sets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(labels: &[&str]) -> Vec<CompactString> {
        labels.iter().map(|l| CompactString::from(*l)).collect()
    }

    #[test]
    fn sets_and_names_are_interned_once() {
        let mut labels = Labels::default();
        assert_eq!(labels.intern_set(&[]), LabelSetId::EMPTY);
        let person = labels.intern_set(&set(&["person"]));
        let admin = labels.intern_set(&set(&["person", "admin"]));
        assert_ne!(person, admin);
        assert_eq!(labels.intern_set(&set(&["person"])), person);
        assert_eq!(labels.intern_set(&set(&["person", "admin"])), admin);
        // The order is part of the set (the first label is the primary one).
        assert_ne!(labels.intern_set(&set(&["admin", "person"])), admin);
        assert_eq!(labels.set(admin), set(&["person", "admin"]).as_slice());
        assert!(labels.set(LabelSetId::EMPTY).is_empty());

        assert_eq!(labels.intern_name(None), LabelId::NONE);
        assert_eq!(labels.name(LabelId::NONE), None);
        let knows = labels.intern_name(Some("knows"));
        assert_eq!(labels.intern_name(Some("knows")), knows);
        assert_eq!(labels.name(knows).map(|n| n.as_str()), Some("knows"));
        assert!(labels.heap_bytes() > 0);
    }

    #[test]
    fn many_labels_survive_the_table_growing() {
        let mut labels = Labels::default();
        let ids: Vec<(LabelId, LabelSetId)> = (0..5000)
            .map(|i| {
                let name = format!("label_{i}");
                (
                    labels.intern_name(Some(&name)),
                    labels.intern_set(&set(&[name.as_str(), "shared"])),
                )
            })
            .collect();
        for (i, (name, set_id)) in ids.iter().enumerate() {
            let text = format!("label_{i}");
            assert_eq!(labels.intern_name(Some(&text)), *name);
            assert_eq!(labels.name_str(*name), Some(text.as_str()));
            assert_eq!(labels.intern_set(&set(&[text.as_str(), "shared"])), *set_id);
            assert_eq!(labels.set(*set_id)[0], text.as_str());
        }
    }

    #[test]
    fn the_kept_set_bytes_equal_a_walk() {
        let mut labels = Labels::default();
        for i in 0..50 {
            let long = format!("a label longer than the inline limit {i}");
            labels.intern_set(&set(&[long.as_str(), "short"]));
            labels.intern_set(&set(&["short"]));
        }
        let walked: u64 = labels.sets.iter().map(|s| set_bytes(s)).sum();
        assert_eq!(labels.set_content_bytes, walked);
    }
}
