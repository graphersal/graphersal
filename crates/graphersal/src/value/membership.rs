//! Hash-assisted membership for `within`/`without` (issue #21).
//!
//! `where(P.without("x"))` against an `aggregate("x")` bucket and `has(key, P.within([..]))`
//! with a long literal list used to compare the tested value with every member, so the
//! visited-set BFS idiom (`aggregate("x").repeat(__.both().where(P.without("x")).aggregate("x"))`)
//! was quadratic. The structures here narrow a test down to the members that can be equal.
//!
//! **Equality never changes.** A hash only selects candidates; every candidate is still
//! confirmed by the exact comparison the linear scan applies. The hashes are therefore built so
//! that two values the comparison calls equal always hash alike (numbers through their `f64`
//! value, because `1` equals `1.0`; a vector only through its element type and dimension); values
//! that cannot be hashed safely (containers, paths, a lazily read property of a plan that
//! writes) stay on a short list that is compared one by one, as before.
//!
//! Both structures are built lazily, only for a collection of at least [`MIN_INDEXED`] members,
//! so a `within` over a handful of values runs the plain scan.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use fnv::{FnvBuildHasher, FnvHasher};

use crate::graph::GraphStorage;
use crate::value::predicate::Scalar;
use crate::value::{N, P, PValue, TraverserValue};

/// The smallest collection that gets a hash index; a shorter one is scanned.
pub(crate) const MIN_INDEXED: usize = 16;

/// The end of a candidate chain.
const END: u32 = u32::MAX;

/// The most members an index covers (positions are `u32`, `END` excluded).
const MAX_INDEXED: usize = (u32::MAX - 1) as usize;

/// The hash of a scalar, consistent with the scalar equality of `within` (`Scalar::equals`
/// for a side effect, `compare_scalar` for a literal operand): equal scalars hash alike.
pub(crate) fn scalar_hash(scalar: &Scalar<'_>) -> u64 {
    let mut state = FnvHasher::default();
    match scalar {
        Scalar::Null => 0u8.hash(&mut state),
        Scalar::Bool(b) => {
            1u8.hash(&mut state);
            b.hash(&mut state);
        }
        Scalar::Number(n) => {
            // `1` equals `1.0` (an integer compares as its `f64`): hash every number through its
            // `f64` value, `-0.0` as `0.0`. A `NaN` equals nothing, so its hash does not matter.
            let value = match &n.n {
                N::NegInt(i) => *i as f64,
                N::Float(f) => f.0,
            };
            let value = if value == 0.0 { 0.0 } else { value };
            2u8.hash(&mut state);
            value.to_bits().hash(&mut state);
        }
        Scalar::Str(text) => {
            3u8.hash(&mut state);
            text.as_ref().hash(&mut state);
        }
        Scalar::Uuid(u) => {
            4u8.hash(&mut state);
            u.hash(&mut state);
        }
        Scalar::Temporal(t) => {
            // `Temporal`'s hash follows its equality (a `datetime` by its instant), and two
            // temporal values are equal only within one type.
            5u8.hash(&mut state);
            t.hash(&mut state);
        }
        Scalar::Vector(v) => {
            // Vectors compare by their total order, whose `Equal` treats `-0.0` as `0.0`: only
            // the parts that equality needs to be the same are hashed.
            6u8.hash(&mut state);
            v.element().hash(&mut state);
            v.dimension().hash(&mut state);
        }
    }
    state.finish()
}

/// The hash of an element handle (a vertex, an edge, a `properties()` element), consistent with
/// its identity equality.
pub(crate) fn element_hash<S: GraphStorage>(value: &TraverserValue<S>) -> u64 {
    let mut state = FnvHasher::default();
    value.hash(&mut state);
    state.finish()
}

/// Where a member of a side-effect collection is looked up.
pub(crate) enum MemberClass {
    /// A scalar, by [`scalar_hash`].
    Scalar(u64),
    /// An element handle, by [`element_hash`].
    Element(u64),
    /// Compared one by one: a container, a path, a value whose reading failed, or a lazily read
    /// property of a plan that may change it.
    Linear,
}

/// The index of one append-only side-effect collection (an `aggregate()`/`store()` list or a
/// set bucket): which members can equal a tested value.
///
/// Members `0..indexed` are classified; the collection only ever grows at its end, so a test
/// classifies the members added since the last one (amortized O(1) per member). A collection
/// shorter than `indexed` was replaced: the index starts over.
#[derive(Default)]
pub(crate) struct MemberIndex {
    indexed: usize,
    /// Per member: the previous member of the same chain, [`END`] for the first.
    next: Vec<u32>,
    /// The newest scalar member per hash.
    scalars: HashMap<u64, u32, FnvBuildHasher>,
    /// The newest element member per hash.
    elements: HashMap<u64, u32, FnvBuildHasher>,
    /// Members compared one by one.
    linear: Vec<u32>,
}

impl MemberIndex {
    /// How many leading members are classified.
    pub(crate) fn indexed(&self) -> usize {
        self.indexed
    }

    /// Whether a collection of `len` members can be indexed.
    pub(crate) fn covers(len: usize) -> bool {
        (MIN_INDEXED..=MAX_INDEXED).contains(&len)
    }

    /// Forgets every member.
    pub(crate) fn reset(&mut self) {
        *self = MemberIndex::default();
    }

    /// Classifies the next member (position [`Self::indexed`]).
    pub(crate) fn push(&mut self, class: MemberClass) {
        // `covers` bounds every collection that reaches here.
        let position = u32::try_from(self.indexed).unwrap_or(END);
        let previous = match class {
            MemberClass::Scalar(hash) => self.scalars.insert(hash, position),
            MemberClass::Element(hash) => self.elements.insert(hash, position),
            MemberClass::Linear => {
                self.linear.push(position);
                None
            }
        };
        self.next.push(previous.unwrap_or(END));
        self.indexed += 1;
    }

    /// The scalar members that can equal a scalar of hash `hash`.
    pub(crate) fn scalar_candidates(&self, hash: u64) -> Chain<'_> {
        Chain {
            next: &self.next,
            at: self.scalars.get(&hash).copied().unwrap_or(END),
        }
    }

    /// The element members that can equal an element of hash `hash`.
    pub(crate) fn element_candidates(&self, hash: u64) -> Chain<'_> {
        Chain {
            next: &self.next,
            at: self.elements.get(&hash).copied().unwrap_or(END),
        }
    }

    /// The members compared one by one, in collection order.
    pub(crate) fn linear(&self) -> &[u32] {
        &self.linear
    }

    /// The heap bytes the index holds (an estimate from the capacities).
    pub(crate) fn heap_bytes(&self) -> u64 {
        let entry = std::mem::size_of::<(u64, u32)>() + 1;
        let bytes = (self.next.capacity() + self.linear.capacity()) * 4
            + (self.scalars.capacity() + self.elements.capacity()) * entry;
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }
}

/// The positions of one hash chain, newest first.
pub(crate) struct Chain<'a> {
    next: &'a [u32],
    at: u32,
}

impl Iterator for Chain<'_> {
    type Item = usize;
    fn next(&mut self) -> Option<usize> {
        if self.at == END {
            return None;
        }
        let position = self.at as usize;
        self.at = self.next.get(position).copied().unwrap_or(END);
        Some(position)
    }
}

/// The [`MemberIndex`] a side-effect bucket carries; built on the first `within` test that
/// needs it. A clone starts without one (it is rebuilt on demand).
#[derive(Default)]
pub(crate) struct MemberIndexCell(Option<Box<MemberIndex>>);

impl MemberIndexCell {
    /// The index, created empty on first use.
    pub(crate) fn get_or_default(&mut self) -> &mut MemberIndex {
        self.0.get_or_insert_with(Box::default)
    }
}

impl Clone for MemberIndexCell {
    fn clone(&self) -> Self {
        MemberIndexCell(None)
    }
}

/// The operands of a long literal `within`/`without` list, hashed once.
pub(crate) struct LiteralSet {
    values: Vec<PValue>,
    /// Per operand: the previous operand of the same chain, [`END`] for the first.
    next: Vec<u32>,
    /// The last operand per hash.
    heads: HashMap<u64, u32, FnvBuildHasher>,
}

impl LiteralSet {
    /// The set of `operands`; `None` for a list shorter than [`MIN_INDEXED`] (scanned instead).
    fn build<B: Into<PValue> + Clone>(operands: &[B]) -> Option<LiteralSet> {
        if !MemberIndex::covers(operands.len()) {
            return None;
        }
        let mut set = LiteralSet {
            values: Vec::with_capacity(operands.len()),
            next: Vec::with_capacity(operands.len()),
            heads: HashMap::with_capacity_and_hasher(operands.len(), FnvBuildHasher::default()),
        };
        for operand in operands {
            let value: PValue = operand.clone().into();
            let hash = scalar_hash(&Scalar::of_operand(&value));
            let position = u32::try_from(set.values.len()).unwrap_or(END);
            set.next
                .push(set.heads.insert(hash, position).unwrap_or(END));
            set.values.push(value);
        }
        Some(set)
    }

    /// The operands that can equal a scalar of hash `hash`.
    pub(crate) fn candidates(&self, hash: u64) -> impl Iterator<Item = &PValue> {
        Chain {
            next: &self.next,
            at: self.heads.get(&hash).copied().unwrap_or(END),
        }
        .filter_map(|position| self.values.get(position))
    }
}

/// A step's cache of the [`LiteralSet`] of its predicate: built on the first test, kept for the
/// step's lifetime. Only a top-level `within`/`without` with at least [`MIN_INDEXED`] operands
/// gets one; the set remembers the operand list it was built from and is used only for that list.
/// A clone starts empty.
#[derive(Default)]
pub(crate) struct LiteralSetCache(OnceLock<Option<(usize, usize, LiteralSet)>>);

impl Clone for LiteralSetCache {
    fn clone(&self) -> Self {
        LiteralSetCache::default()
    }
}

impl LiteralSetCache {
    /// The set of `predicate` and whether it is negated (`without`), or `None` when the plain
    /// evaluation applies.
    pub(crate) fn get<B: Into<PValue> + Clone>(
        &self,
        predicate: &P<B>,
    ) -> Option<(&LiteralSet, bool)> {
        let (operands, negated) = match predicate {
            P::WithIn(operands) => (operands, false),
            P::WithOut(operands) => (operands, true),
            _ => return None,
        };
        if operands.len() < MIN_INDEXED {
            return None;
        }
        let key = (operands.as_ptr() as usize, operands.len());
        let cached = self
            .0
            .get_or_init(|| LiteralSet::build(operands).map(|set| (key.0, key.1, set)));
        match cached {
            Some((ptr, len, set)) if (*ptr, *len) == key => Some((set, negated)),
            _ => None,
        }
    }
}
