//! The element handles of [`LayeredGraph`](super::LayeredGraph): a slot tagged by bit 31 plus a
//! generation, 8 bytes like [`VertexHandle`]/[`EdgeHandle`].
//!
//! Bit 31 of the slot clear: the handle names a slot of the base and maps 1:1 to the base's
//! handle (same slot, same generation), no lookup. Bit 31 set: the low 31 bits are a slot of the
//! layer's own delta store. A base has fewer than 2^31 slots of each kind
//! ([`LayeredGraph::over`](super::LayeredGraph::over) refuses a larger one), so the two spaces
//! never overlap.

use std::num::NonZeroU32;

use crate::graph::topology::{EdgeIndex, NodeIndex};
use crate::graph::{EdgeHandle, VertexHandle};

/// The tag bit of a delta slot.
pub(crate) const DELTA_BIT: u32 = 1 << 31;

/// The first slot count a base may NOT have (of either kind): every base slot index stays below
/// [`DELTA_BIT`].
pub(crate) const MAX_BASE_SLOTS: usize = DELTA_BIT as usize;

/// An opaque handle to one vertex of a [`LayeredGraph`](super::LayeredGraph).
///
/// The rules of [`VertexHandle`] apply: opaque (no public constructor, no public fields, the
/// representation may change in any release), storage-local (meaningful only for the layer that
/// returned it), valid only while the vertex exists (a stale handle never resolves to another
/// vertex), never persisted (store the vertex id instead).
///
/// `Debug` prints `v[slot#generation]` for a vertex of the base and `v[+slot#generation]` for
/// one the layer added, for diagnostics only.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayeredVertex {
    slot: u32,
    generation: NonZeroU32,
}

/// An opaque handle to one edge of a [`LayeredGraph`](super::LayeredGraph).
///
/// The same rules as for [`LayeredVertex`] apply. `Debug` prints `e[slot#generation]` for an edge
/// of the base and `e[+slot#generation]` for one the layer added.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayeredEdge {
    slot: u32,
    generation: NonZeroU32,
}

impl LayeredVertex {
    /// The layer's handle of a base vertex: same slot, same generation.
    #[inline]
    pub(crate) fn from_base(handle: VertexHandle) -> Self {
        Self {
            slot: handle.index().slot(),
            generation: handle.generation(),
        }
    }

    /// `true` for a vertex of the layer's delta store.
    #[inline]
    pub(crate) fn is_delta(self) -> bool {
        self.slot & DELTA_BIT != 0
    }

    /// The raw slot (bit 31 included).
    #[inline]
    pub(crate) fn slot(self) -> u32 {
        self.slot
    }

    /// The base handle with this slot and generation. For a delta handle the slot is at or
    /// above 2^31, which no base slot reaches: the base reads it as stale (no vertex, no edges).
    #[inline]
    pub(crate) fn to_base(self) -> VertexHandle {
        VertexHandle::new(NodeIndex::from_slot(self.slot), self.generation)
    }

    /// The layer's handle of a vertex of the delta store (`handle` names a slot of the delta's
    /// own topology, below 2^31): the slot tagged with bit 31, same generation.
    #[inline]
    pub(crate) fn from_delta(handle: VertexHandle) -> Self {
        Self {
            slot: handle.index().slot() | DELTA_BIT,
            generation: handle.generation(),
        }
    }

    /// The handle in the delta store's own slot space: the tag bit cleared. Meaningful for a
    /// delta handle; a base handle maps to a delta slot of the same number, so callers route
    /// on [`Self::is_delta`] first.
    #[inline]
    pub(crate) fn to_delta(self) -> VertexHandle {
        VertexHandle::new(
            NodeIndex::from_slot(self.slot & !DELTA_BIT),
            self.generation,
        )
    }
}

#[cfg(test)]
impl LayeredVertex {
    /// A delta handle for unit tests (no delta element exists yet to hand one out).
    pub(crate) fn delta_for_test(slot: u32) -> Self {
        Self {
            slot: slot | DELTA_BIT,
            generation: NonZeroU32::MIN,
        }
    }
}

#[cfg(test)]
impl LayeredEdge {
    /// A delta handle for unit tests; see [`LayeredVertex::delta_for_test`].
    pub(crate) fn delta_for_test(slot: u32) -> Self {
        Self {
            slot: slot | DELTA_BIT,
            generation: NonZeroU32::MIN,
        }
    }
}

impl LayeredEdge {
    /// The layer's handle of a base edge: same slot, same generation.
    #[inline]
    pub(crate) fn from_base(handle: EdgeHandle) -> Self {
        Self {
            slot: handle.index().slot(),
            generation: handle.generation(),
        }
    }

    /// `true` for an edge of the layer's delta store.
    #[inline]
    pub(crate) fn is_delta(self) -> bool {
        self.slot & DELTA_BIT != 0
    }

    /// The raw slot (bit 31 included).
    #[inline]
    pub(crate) fn slot(self) -> u32 {
        self.slot
    }

    /// The base handle with this slot and generation (stale in the base for a delta handle, see
    /// [`LayeredVertex::to_base`]).
    #[inline]
    pub(crate) fn to_base(self) -> EdgeHandle {
        EdgeHandle::new(EdgeIndex::from_slot(self.slot), self.generation)
    }

    /// The layer's handle of an edge of the delta store; see [`LayeredVertex::from_delta`].
    #[inline]
    pub(crate) fn from_delta(handle: EdgeHandle) -> Self {
        Self {
            slot: handle.index().slot() | DELTA_BIT,
            generation: handle.generation(),
        }
    }

    /// The handle in the delta store's own slot space; see [`LayeredVertex::to_delta`].
    #[inline]
    pub(crate) fn to_delta(self) -> EdgeHandle {
        EdgeHandle::new(
            EdgeIndex::from_slot(self.slot & !DELTA_BIT),
            self.generation,
        )
    }
}

impl std::fmt::Debug for LayeredVertex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let added = if self.is_delta() { "+" } else { "" };
        write!(
            f,
            "v[{added}{}#{}]",
            self.slot & !DELTA_BIT,
            self.generation
        )
    }
}

impl std::fmt::Debug for LayeredEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let added = if self.is_delta() { "+" } else { "" };
        write!(
            f,
            "e[{added}{}#{}]",
            self.slot & !DELTA_BIT,
            self.generation
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_stay_8_bytes() {
        assert_eq!(size_of::<LayeredVertex>(), 8);
        assert_eq!(size_of::<LayeredEdge>(), 8);
        assert_eq!(size_of::<Option<LayeredVertex>>(), 8);
    }

    #[test]
    fn base_handles_round_trip_and_are_not_delta() {
        let vertex = VertexHandle::for_test(7);
        let layered = LayeredVertex::from_base(vertex);
        assert!(!layered.is_delta());
        assert_eq!(layered.to_base(), vertex);
        assert_eq!(format!("{layered:?}"), "v[7#1]");
        let edge = EdgeHandle::for_test(3);
        let layered = LayeredEdge::from_base(edge);
        assert!(!layered.is_delta());
        assert_eq!(layered.to_base(), edge);
        assert_eq!(format!("{layered:?}"), "e[3#1]");
    }

    #[test]
    fn delta_handles_are_tagged() {
        let vertex = LayeredVertex::delta_for_test(5);
        assert!(vertex.is_delta());
        assert_eq!(format!("{vertex:?}"), "v[+5#1]");
        let edge = LayeredEdge::delta_for_test(0);
        assert!(edge.is_delta());
        assert_eq!(format!("{edge:?}"), "e[+0#1]");
    }
}
