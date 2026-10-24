//! Generational element handles of [`TraversalGraph`](crate::TraversalGraph).
//!
//! A handle is a storage slot plus the generation the slot had when the element was created. The
//! storage bumps a slot's generation when it frees the slot, so a handle taken before a drop never
//! resolves to the element that later reuses the slot: it is *stale* and reads as removed
//! (`book/src/users_guide/transactions.md`).

use std::num::NonZeroU32;

use crate::graph::topology::{EdgeIndex, NodeIndex};

/// The first generation of a never-used slot.
pub(crate) const FIRST_GENERATION: NonZeroU32 = NonZeroU32::MIN;

/// The generation a slot gets when it is freed: `+1`, wrapping from `u32::MAX` to `1` (never 0).
#[inline]
pub(crate) fn next_generation(generation: NonZeroU32) -> NonZeroU32 {
    generation.checked_add(1).unwrap_or(FIRST_GENERATION)
}

/// An opaque handle to one vertex of a [`TraversalGraph`](crate::TraversalGraph).
///
/// - **Opaque:** no public constructor, no public fields; the representation (today a slot index
///   and a generation) may change in any release.
/// - **Storage-local:** it is meaningful only for the graph that returned it. A handle of another
///   graph may resolve to an unrelated vertex there or to nothing.
/// - **Valid only while the vertex exists:** after the vertex is dropped the handle is *stale*.
///   A stale handle never resolves to another vertex, even when a new vertex reuses the slot:
///   reads through it see no properties, no labels and no edges, `id()`/`label()` and
///   materialization fail with [`GraphError::ElementRemoved`](crate::error::GraphError), and
///   writes through it fail with the same error.
/// - **Never persisted:** it is not an id. Store and exchange the vertex id (`id()`) instead.
///
/// `Debug` prints `v[slot#generation]`, for diagnostics only.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VertexHandle {
    slot: u32,
    generation: NonZeroU32,
}

/// An opaque handle to one edge of a [`TraversalGraph`](crate::TraversalGraph).
///
/// The same rules as for [`VertexHandle`] apply: opaque, storage-local, valid only while the edge
/// exists (a stale handle never resolves to another edge), never persisted.
///
/// `Debug` prints `e[slot#generation]`, for diagnostics only.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeHandle {
    slot: u32,
    generation: NonZeroU32,
}

impl VertexHandle {
    #[inline]
    pub(crate) fn new(index: NodeIndex, generation: NonZeroU32) -> Self {
        Self {
            slot: index.index() as u32,
            generation,
        }
    }

    /// The topology slot; NOT validated, see `TraversalGraph::resolve_vertex`.
    #[inline]
    pub(crate) fn index(self) -> NodeIndex {
        NodeIndex::new(self.slot as usize)
    }

    #[inline]
    pub(crate) fn generation(self) -> NonZeroU32 {
        self.generation
    }

    /// A handle for crate-internal unit tests that only need distinct, hashable values.
    #[cfg(test)]
    pub(crate) fn for_test(slot: u32) -> Self {
        Self {
            slot,
            generation: FIRST_GENERATION,
        }
    }
}

impl EdgeHandle {
    #[inline]
    pub(crate) fn new(index: EdgeIndex, generation: NonZeroU32) -> Self {
        Self {
            slot: index.index() as u32,
            generation,
        }
    }

    /// The topology slot; NOT validated, see `TraversalGraph::resolve_edge`.
    #[inline]
    pub(crate) fn index(self) -> EdgeIndex {
        EdgeIndex::new(self.slot as usize)
    }

    #[inline]
    pub(crate) fn generation(self) -> NonZeroU32 {
        self.generation
    }

    /// A handle for crate-internal unit tests that only need distinct, hashable values.
    #[cfg(test)]
    pub(crate) fn for_test(slot: u32) -> Self {
        Self {
            slot,
            generation: FIRST_GENERATION,
        }
    }
}

impl std::fmt::Debug for VertexHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v[{}#{}]", self.slot, self.generation)
    }
}

impl std::fmt::Debug for EdgeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "e[{}#{}]", self.slot, self.generation)
    }
}

/// The generations of an edge and of its two endpoints, recorded when the edge is created.
///
/// An endpoint cannot be dropped without its edges, so the endpoint generations stay correct for
/// the whole life of the edge: adjacency and `edge_endpoints` build vertex handles from them
/// without loading the vertex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EdgeGenerations {
    pub(crate) own: NonZeroU32,
    pub(crate) source: NonZeroU32,
    pub(crate) target: NonZeroU32,
}

/// One generation per slot of one element kind: the generation the slot's NEXT (or current)
/// occupant gets. Bumped when the slot is freed.
#[derive(Debug, Clone, Default)]
pub(crate) struct Generations {
    slots: Vec<NonZeroU32>,
}

impl Generations {
    /// The generation for a slot that is about to be occupied.
    #[inline]
    pub(crate) fn current(&mut self, slot: usize) -> NonZeroU32 {
        if slot >= self.slots.len() {
            self.slots.resize(slot + 1, FIRST_GENERATION);
        }
        self.slots[slot]
    }

    /// Marks a slot as freed: its next occupant gets a new generation.
    #[inline]
    pub(crate) fn bump(&mut self, slot: usize) {
        if let Some(generation) = self.slots.get_mut(slot) {
            *generation = next_generation(*generation);
        }
    }

    /// Heap bytes of the generation array (for `memory_usage()`).
    pub(crate) fn heap_bytes(&self) -> u64 {
        self.slots.capacity() as u64 * std::mem::size_of::<NonZeroU32>() as u64
    }

    #[inline]
    pub(crate) fn shrink_to_fit(&mut self) {
        self.slots.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_wraps_and_skips_zero() {
        let max = NonZeroU32::new(u32::MAX).expect("non-zero");
        assert_eq!(next_generation(max), FIRST_GENERATION);
        assert_eq!(next_generation(FIRST_GENERATION).get(), 2);
    }

    #[test]
    fn handles_are_eight_bytes_with_a_niche() {
        assert_eq!(std::mem::size_of::<VertexHandle>(), 8);
        assert_eq!(std::mem::size_of::<Option<VertexHandle>>(), 8);
        assert_eq!(std::mem::size_of::<EdgeHandle>(), 8);
    }

    #[test]
    fn debug_is_slot_and_generation() {
        let v = VertexHandle::new(NodeIndex::new(3), NonZeroU32::new(2).expect("non-zero"));
        assert_eq!(format!("{v:?}"), "v[3#2]");
        let e = EdgeHandle::new(EdgeIndex::new(0), FIRST_GENERATION);
        assert_eq!(format!("{e:?}"), "e[0#1]");
    }

    #[test]
    fn bump_changes_the_generation_of_a_slot() {
        let mut generations = Generations::default();
        let first = generations.current(5);
        generations.bump(5);
        assert_ne!(generations.current(5), first);
        assert_eq!(generations.current(0), FIRST_GENERATION);
    }
}
