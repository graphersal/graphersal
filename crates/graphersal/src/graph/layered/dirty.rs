//! The dirty bitsets of a [`LayeredGraph`](super::LayeredGraph): one bit per base slot.
//!
//! A clear bit means the base element is untouched by the layer: every read goes straight to
//! the base, no hash map, no delta lookup. A set bit means the delta holds the element's state
//! (removed, or a copied-up image). Allocated for every base slot when the layer is created,
//! all clear.

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

    /// `true` once any bit was set (no write sets one yet).
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
}
