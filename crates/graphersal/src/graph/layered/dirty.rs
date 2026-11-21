//! The dirty bitsets of a [`LayeredGraph`](super::LayeredGraph): one bit per base slot.
//!
//! A clear bit means the base element is untouched by the layer: every read goes straight to
//! the base, no hash map, no delta lookup. A set bit means "consult the delta for this
//! element": for a vertex, it may have a stub with new incident edges (S1.4), be removed or
//! have a copied-up image; for an edge, be removed or have an image. A set bit alone never
//! means the element changed: the delta answers that. Allocated for every base slot when the
//! layer is created, all clear.

/// One bit per base slot of one element kind.
#[derive(Debug, Clone, Default)]
pub(crate) struct SlotBits {
    words: Vec<u64>,
    /// `true` once any bit was set: while `false` a read skips the word load entirely (a scan
    /// captures it once, by value, so a clean layer pays one predictable branch per item).
    any: bool,
}

impl SlotBits {
    /// `slots` clear bits.
    pub(crate) fn new(slots: usize) -> Self {
        Self {
            words: vec![0; slots.div_ceil(64)],
            any: false,
        }
    }

    /// `true` once any bit was set.
    #[inline]
    pub(crate) fn any(&self) -> bool {
        self.any
    }

    /// `true` iff the bit of base slot `slot` is set. A slot beyond the allocated bits (a delta
    /// slot, or a forged handle) reads as clear: the caller routes delta slots before asking.
    #[inline]
    pub(crate) fn contains(&self, slot: u32) -> bool {
        self.any && self.word_contains(slot)
    }

    /// Sets the bit of base slot `slot` and [`Self::any`] with it (the one way to set a bit, so
    /// the two never disagree). A slot beyond the allocated bits is ignored: the bits cover
    /// every base slot, and the caller only sets the bit of a live base element.
    #[inline]
    pub(crate) fn set(&mut self, slot: u32) {
        if let Some(word) = self.words.get_mut((slot >> 6) as usize) {
            *word |= 1 << (slot & 63);
            self.any = true;
        }
    }

    /// [`Self::set`] for a set that starts empty and grows on demand (the tombstones: a layer
    /// that never removes a base element allocates nothing for them). `slot` must be a base
    /// slot (below 2^31): the words grow to cover it.
    #[inline]
    pub(crate) fn set_growing(&mut self, slot: u32) {
        let word = (slot >> 6) as usize;
        if self.words.len() <= word {
            self.words.resize(word + 1, 0);
        }
        self.set(slot);
    }

    /// Clears the bit of `slot` (nothing beyond the allocated bits). [`Self::any`] stays set: it
    /// only lets a never-touched set skip the word load, so a stale `true` costs one load.
    /// Never used on the dirty bits themselves (they are never cleared, plan invariant S1.5),
    /// only on the sets read behind them (images, lost labels, relabeled edges).
    #[inline]
    pub(crate) fn clear(&mut self, slot: u32) {
        if let Some(word) = self.words.get_mut((slot >> 6) as usize) {
            *word &= !(1 << (slot & 63));
        }
    }

    /// The bit itself, without the [`Self::any`] shortcut.
    #[inline]
    fn word_contains(&self, slot: u32) -> bool {
        match self.words.get((slot >> 6) as usize) {
            Some(word) => (word >> (slot & 63)) & 1 != 0,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bits_are_clear_and_cover_every_slot() {
        let bits = SlotBits::new(130);
        assert_eq!(bits.words.len(), 3);
        assert!((0..130).all(|slot| !bits.contains(slot)));
        assert!(!bits.contains(u32::MAX));
        assert!(!SlotBits::default().contains(0));
    }

    #[test]
    fn set_marks_one_slot_and_any() {
        let mut bits = SlotBits::new(130);
        bits.set(129);
        assert!(bits.any());
        assert!(bits.contains(129));
        assert!((0..129).all(|slot| !bits.contains(slot)));
        let mut empty = SlotBits::default();
        empty.set(0);
        assert!(!empty.any(), "a slot beyond the bits is ignored");
    }

    #[test]
    fn set_growing_covers_the_slot() {
        let mut bits = SlotBits::default();
        assert!(!bits.contains(200));
        bits.set_growing(200);
        assert!(bits.any() && bits.contains(200));
        assert_eq!(bits.words.len(), 4);
        assert!((0..200).all(|slot| !bits.contains(slot)));
        bits.set_growing(3);
        assert!(bits.contains(3) && bits.contains(200));
        assert_eq!(bits.words.len(), 4, "no growth for a covered slot");
    }

    #[test]
    fn clear_unsets_one_slot_only() {
        let mut bits = SlotBits::default();
        bits.set_growing(70);
        bits.set_growing(71);
        bits.clear(70);
        bits.clear(5000);
        assert!(!bits.contains(70) && bits.contains(71));
        bits.clear(71);
        assert!(!bits.contains(71) && bits.any(), "any stays set");
    }
}
