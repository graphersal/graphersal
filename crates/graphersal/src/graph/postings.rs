//! The postings of a property index: the slots of the elements that hold one key.
//!
//! An index keeps each key ONCE (`BTreeMap<IndexKey, Postings>`, `property_index.rs`) and its
//! elements as a sorted set of SLOTS (`u32`, the topology's slot type). The generation is not
//! stored: maintenance is exact (every mutation and undo path takes an element's entries out and
//! puts them back), so at most one live element owns a slot an index names, and a lookup reads the
//! generation from the slot when it produces a candidate.
//!
//! The set is hybrid so near-unique keys do not pay for a container: one slot is kept inline
//! ([`Postings::One`]); from the second slot on the slots live in a [`Runs`] of the index's
//! [`RunsStore`] and the map value names it ([`Postings::Many`]), so a map value is 8 bytes either
//! way. A run is a sorted `Vec<u32>` ([`Runs::Flat`]) and past [`CHUNK`] slots sorted chunks of
//! at most [`CHUNK`] slots each ([`Runs::Chunked`]), so an insert or a remove moves at most one
//! chunk (16 KiB), never the whole set. Measured (1M slots, requested bytes): a `BTreeSet<u32>`
//! costs 9-12.6 B per slot, these runs 4.0-5.2 B after a build (a build shrinks every run,
//! [`RunsStore::shrink`]) and at most about 8 B under later writes.

/// The most slots one run holds.
pub(crate) const CHUNK: usize = 4096;

/// The slots of the elements holding one key, ascending, never empty: the map value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Postings {
    /// Exactly one slot, inline.
    One(u32),
    /// Two or more slots: the position of their [`Runs`] in the index's [`RunsStore`].
    Many(u32),
}

/// Two or more sorted slots.
#[derive(Debug, Clone)]
pub(crate) enum Runs {
    /// `2..=CHUNK` slots in one sorted run (empty in a free position of the store).
    Flat(Vec<u32>),
    /// Two or more non-empty sorted chunks of at most [`CHUNK`] slots each, ascending across
    /// chunks.
    Chunked(Vec<Vec<u32>>),
}

/// The runs of one index, by position, with the free positions reused first.
#[derive(Debug, Clone, Default)]
pub(crate) struct RunsStore {
    runs: Vec<Runs>,
    free: Vec<u32>,
}

/// What [`RunsStore::remove`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Removed {
    /// The slot was not there.
    Absent,
    /// The slot was removed, others remain.
    Removed,
    /// The slot was the only one: the caller drops the key.
    Emptied,
}

impl RunsStore {
    /// Adds `slot` to `postings`; `false` when it was there already.
    pub(crate) fn insert(&mut self, postings: &mut Postings, slot: u32) -> bool {
        match *postings {
            Postings::One(only) => {
                if only == slot {
                    return false;
                }
                let mut run = Vec::with_capacity(2);
                run.push(only.min(slot));
                run.push(only.max(slot));
                *postings = Postings::Many(self.allocate(Runs::Flat(run)));
                true
            }
            Postings::Many(at) => match self.runs.get_mut(at as usize) {
                Some(runs) => runs.insert(slot),
                None => false,
            },
        }
    }

    /// Removes `slot` from `postings` (back to [`Postings::One`] when one slot is left).
    pub(crate) fn remove(&mut self, postings: &mut Postings, slot: u32) -> Removed {
        match *postings {
            Postings::One(only) if only == slot => Removed::Emptied,
            Postings::One(_) => Removed::Absent,
            Postings::Many(at) => {
                let Some(runs) = self.runs.get_mut(at as usize) else {
                    return Removed::Absent;
                };
                if !runs.remove(slot) {
                    return Removed::Absent;
                }
                if let Runs::Flat(run) = runs
                    && let [last] = run.as_slice()
                {
                    *postings = Postings::One(*last);
                    *runs = Runs::Flat(Vec::new());
                    self.free.push(at);
                    if self.free.len() == self.runs.len() {
                        // No run left: the table goes too.
                        *self = RunsStore::default();
                    }
                }
                Removed::Removed
            }
        }
    }

    fn allocate(&mut self, runs: Runs) -> u32 {
        if let Some(at) = self.free.pop()
            && let Some(free) = self.runs.get_mut(at as usize)
        {
            *free = runs;
            return at;
        }
        // At most one run per two slots, and slots are `u32`: the position fits.
        let at = self.runs.len() as u32;
        self.runs.push(runs);
        at
    }

    /// The slots of `postings`, ascending.
    pub(crate) fn iter(&self, postings: &Postings) -> Iter<'_> {
        match *postings {
            Postings::One(slot) => Iter::One(Some(slot)),
            Postings::Many(at) => match self.runs.get(at as usize) {
                Some(Runs::Flat(run)) => Iter::Flat(run.iter()),
                Some(Runs::Chunked(chunks)) => Iter::Chunked(chunks.iter().flatten()),
                None => Iter::One(None),
            },
        }
    }

    /// The number of slots of `postings`.
    pub(crate) fn len(&self, postings: &Postings) -> usize {
        match *postings {
            Postings::One(_) => 1,
            Postings::Many(at) => match self.runs.get(at as usize) {
                Some(Runs::Flat(run)) => run.len(),
                Some(Runs::Chunked(chunks)) => chunks.iter().map(Vec::len).sum(),
                None => 0,
            },
        }
    }

    /// The heap bytes the run of `postings` owns (the capacities of its vectors; a
    /// [`Postings::One`] owns none). The store's own table is [`Self::table_bytes`].
    pub(crate) fn heap_bytes(&self, postings: &Postings) -> u64 {
        let slot = std::mem::size_of::<u32>() as u64;
        match *postings {
            Postings::One(_) => 0,
            Postings::Many(at) => match self.runs.get(at as usize) {
                Some(Runs::Flat(run)) => run.capacity() as u64 * slot,
                Some(Runs::Chunked(chunks)) => {
                    chunks.capacity() as u64 * std::mem::size_of::<Vec<u32>>() as u64
                        + chunks
                            .iter()
                            .map(|chunk| chunk.capacity() as u64 * slot)
                            .sum::<u64>()
                }
                None => 0,
            },
        }
    }

    /// The bytes of the store's table of runs and its free list.
    pub(crate) fn table_bytes(&self) -> u64 {
        (self.runs.capacity() * std::mem::size_of::<Runs>()
            + self.free.capacity() * std::mem::size_of::<u32>()) as u64
    }

    /// Gives back the spare capacity of every run and of the table (after a build).
    pub(crate) fn shrink(&mut self) {
        for runs in &mut self.runs {
            match runs {
                Runs::Flat(run) => run.shrink_to_fit(),
                Runs::Chunked(chunks) => {
                    chunks.iter_mut().for_each(Vec::shrink_to_fit);
                    chunks.shrink_to_fit();
                }
            }
        }
        self.runs.shrink_to_fit();
        self.free.shrink_to_fit();
    }
}

impl Runs {
    fn insert(&mut self, slot: u32) -> bool {
        match self {
            Runs::Flat(run) => {
                let Err(position) = run.binary_search(&slot) else {
                    return false;
                };
                if run.len() < CHUNK {
                    run.insert(position, slot);
                    return true;
                }
                let mut chunks = vec![std::mem::take(run)];
                let inserted = insert_chunked(&mut chunks, slot);
                *self = Runs::Chunked(chunks);
                inserted
            }
            Runs::Chunked(chunks) => insert_chunked(chunks, slot),
        }
    }

    fn remove(&mut self, slot: u32) -> bool {
        match self {
            Runs::Flat(run) => {
                let Ok(position) = run.binary_search(&slot) else {
                    return false;
                };
                run.remove(position);
                give_back(run);
                true
            }
            Runs::Chunked(chunks) => {
                let at = chunks.partition_point(|chunk| chunk.last().is_some_and(|l| *l < slot));
                let Some(chunk) = chunks.get_mut(at) else {
                    return false;
                };
                let Ok(position) = chunk.binary_search(&slot) else {
                    return false;
                };
                chunk.remove(position);
                if chunk.is_empty() {
                    chunks.remove(at);
                    give_back(chunks);
                } else {
                    give_back(chunk);
                }
                if chunks.len() == 1
                    && let Some(run) = chunks.pop()
                {
                    *self = Runs::Flat(run);
                }
                true
            }
        }
    }
}

/// Inserts `slot` into sorted chunks: into the first chunk whose last slot is not below it (or
/// the last chunk); a full chunk gets a new chunk after it when the slot goes past its end (an
/// ascending build fills chunks completely), else it is split in halves.
fn insert_chunked(chunks: &mut Vec<Vec<u32>>, slot: u32) -> bool {
    let at = chunks
        .partition_point(|chunk| chunk.last().is_some_and(|last| *last < slot))
        .min(chunks.len().saturating_sub(1));
    let Some(chunk) = chunks.get_mut(at) else {
        chunks.push(vec![slot]);
        return true;
    };
    let Err(position) = chunk.binary_search(&slot) else {
        return false;
    };
    if chunk.len() < CHUNK {
        chunk.insert(position, slot);
        return true;
    }
    if position == chunk.len() {
        chunks.insert(at + 1, vec![slot]);
        return true;
    }
    let mut right = chunk.split_off(CHUNK / 2);
    if position <= chunk.len() {
        chunk.insert(position, slot);
    } else {
        right.insert(position - chunk.len(), slot);
    }
    chunks.insert(at + 1, right);
    true
}

/// Gives back capacity once a vector uses a quarter of it or less (halving keeps an insert after
/// a remove from reallocating at once).
fn give_back<T>(vector: &mut Vec<T>) {
    if vector.capacity() > 8 && vector.len() * 4 <= vector.capacity() {
        vector.shrink_to(vector.len() * 2);
    }
}

/// The slots of one [`Postings`], ascending.
pub(crate) enum Iter<'a> {
    One(Option<u32>),
    Flat(std::slice::Iter<'a, u32>),
    Chunked(std::iter::Flatten<std::slice::Iter<'a, Vec<u32>>>),
}

impl Iterator for Iter<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        match self {
            Iter::One(slot) => slot.take(),
            Iter::Flat(slots) => slots.next().copied(),
            Iter::Chunked(slots) => slots.next().copied(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(store: &RunsStore, postings: &Postings) -> Vec<u32> {
        store.iter(postings).collect()
    }

    /// Checks the representation invariants.
    fn check(store: &RunsStore, postings: &Postings) {
        if let Postings::Many(at) = postings {
            match &store.runs[*at as usize] {
                Runs::Flat(run) => {
                    assert!(run.len() >= 2 && run.len() <= CHUNK, "flat {}", run.len());
                    assert!(run.windows(2).all(|pair| pair[0] < pair[1]));
                }
                Runs::Chunked(chunks) => {
                    assert!(chunks.len() >= 2);
                    assert!(chunks.iter().all(|c| !c.is_empty() && c.len() <= CHUNK));
                    let all: Vec<u32> = chunks.iter().flatten().copied().collect();
                    assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
                }
            }
        }
        assert_eq!(collect(store, postings).len(), store.len(postings));
    }

    #[test]
    fn postings_are_one_map_word() {
        assert_eq!(std::mem::size_of::<Postings>(), 8);
    }

    #[test]
    fn one_many_chunked_and_back() {
        let mut store = RunsStore::default();
        let mut postings = Postings::One(5);
        assert!(!store.insert(&mut postings, 5));
        assert!(store.insert(&mut postings, 3));
        check(&store, &postings);
        assert_eq!(collect(&store, &postings), vec![3, 5]);
        assert_eq!(store.remove(&mut postings, 4), Removed::Absent);
        assert_eq!(store.remove(&mut postings, 5), Removed::Removed);
        assert_eq!(postings, Postings::One(3));
        assert_eq!(store.table_bytes(), 0, "no run left: no table either");
        assert_eq!(store.remove(&mut postings, 3), Removed::Emptied);
        // A free position is reused.
        let mut other = Postings::One(1);
        assert!(store.insert(&mut other, 2));
        let mut postings = Postings::One(10);
        assert!(store.insert(&mut postings, 11));
        assert_eq!((other, postings), (Postings::Many(0), Postings::Many(1)));
        assert_eq!(store.remove(&mut other, 2), Removed::Removed);
        assert_eq!(store.free, vec![0]);
        assert_eq!(store.remove(&mut postings, 11), Removed::Removed);
        assert_eq!(store.table_bytes(), 0);
        // Past one chunk, descending (splits) and ascending (appends).
        let mut postings = Postings::One(u32::MAX);
        for slot in (0..3 * CHUNK as u32).rev().step_by(2) {
            assert!(store.insert(&mut postings, slot));
        }
        assert_eq!(postings, Postings::Many(0));
        for slot in 3 * CHUNK as u32..5 * CHUNK as u32 {
            assert!(store.insert(&mut postings, slot));
        }
        check(&store, &postings);
        assert!(matches!(store.runs[0], Runs::Chunked(_)));
        let expected: Vec<u32> = (0..3 * CHUNK as u32)
            .rev()
            .step_by(2)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .chain(3 * CHUNK as u32..5 * CHUNK as u32)
            .chain([u32::MAX])
            .collect();
        assert_eq!(collect(&store, &postings), expected);
        let mut shrunk = store.clone();
        shrunk.shrink();
        assert_eq!(collect(&shrunk, &postings), expected);
        assert!(shrunk.heap_bytes(&postings) <= store.heap_bytes(&postings));
        // Remove all but one, checking on the way.
        for (n, slot) in expected.iter().skip(1).enumerate() {
            assert_eq!(store.remove(&mut postings, *slot), Removed::Removed);
            if n % 997 == 0 {
                check(&store, &postings);
            }
        }
        assert_eq!(postings, Postings::One(expected[0]));
        assert_eq!(store.heap_bytes(&postings), 0);
    }
}
