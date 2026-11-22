//! The iterators of layered scans and adjacency lists: ONE concrete type per read, whether the
//! delta takes part or not, shaped so a clean layer costs (almost) nothing per item.
//!
//! The rules behind the shape (S1.12, measured against the plain base):
//! - The decision "does the delta take part" is made once, when the read builds the iterator:
//!   the delta part is `None` and the base filter captures `None` (nothing to hide) for a clean
//!   layer or a clean vertex. Both are loop-invariant values the loop keeps in registers.
//! - No enum over two iterator types and no `Chain` with its two `Option`s: an enum whose
//!   variants overlap in memory, or a merge path called out of line through a pointer to the
//!   iterator, forces the whole iterator onto the stack and turns a tight scan into a state
//!   machine with a load and a store per item.
//! - The base iterators of a `TraversalGraph` are fused in practice (a slice walk, an adjacency
//!   list ending at `EdgeIndex::end()`): once exhausted they keep answering `None`, so
//!   [`BaseThenDelta`] asks the base first on every call without a flag.

/// The base's items, then the delta's (`None`: the delta adds nothing). A scan of a layer lists
/// the base first, then the added elements. The delta part is boxed and advanced out of line
/// ([`next_out_of_line`]): the loop over the base keeps its whole state in registers, since
/// nothing takes the address of the iterator (one allocation per scan, only while the layer
/// holds added elements).
pub(crate) struct BaseThenDelta<'a, A: Iterator> {
    base: A,
    delta: Option<Box<dyn Iterator<Item = A::Item> + 'a>>,
}

impl<'a, A: Iterator> BaseThenDelta<'a, A> {
    /// `base`, then `delta` when there is one.
    #[inline(always)]
    pub(crate) fn new(base: A, delta: Option<impl Iterator<Item = A::Item> + 'a>) -> Self {
        Self {
            base,
            delta: delta.map(|delta| Box::new(delta) as Box<dyn Iterator<Item = A::Item> + 'a>),
        }
    }
}

impl<A: Iterator> Iterator for BaseThenDelta<'_, A> {
    type Item = A::Item;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        match self.base.next() {
            Some(item) => Some(item),
            None => next_out_of_line(self.delta.as_deref_mut()?),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        add_hints(
            self.base.size_hint(),
            self.delta.as_ref().map(|delta| delta.size_hint()),
        )
    }

    #[inline(always)]
    fn fold<Acc, F>(self, init: Acc, mut f: F) -> Acc
    where
        F: FnMut(Acc, Self::Item) -> Acc,
    {
        let acc = self.base.fold(init, &mut f);
        match self.delta {
            Some(delta) => delta.fold(acc, f),
            None => acc,
        }
    }

    #[inline(always)]
    fn count(self) -> usize {
        self.base.count() + self.delta.map_or(0, Iterator::count)
    }
}

/// The `next` of the delta part of a [`BaseThenDelta`], never inlined (it runs once per added
/// element, after the base is exhausted).
#[inline(never)]
fn next_out_of_line<T>(delta: &mut dyn Iterator<Item = T>) -> Option<T> {
    delta.next()
}

/// The delta's items (`None`: the delta adds nothing), then the base's. An adjacency list of a
/// layer lists the added edges first (newest first), as a `TraversalGraph` holding the same
/// writes does.
pub(crate) struct DeltaThenBase<A, B> {
    delta: Option<A>,
    base: B,
}

impl<A, B> DeltaThenBase<A, B> {
    /// `delta` when there is one, then `base`.
    #[inline(always)]
    pub(crate) fn new(delta: Option<A>, base: B) -> Self {
        Self { delta, base }
    }
}

impl<A, B> Iterator for DeltaThenBase<A, B>
where
    A: Iterator,
    B: Iterator<Item = A::Item>,
{
    type Item = A::Item;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(delta) = &mut self.delta {
            match delta.next() {
                Some(item) => return Some(item),
                None => self.delta = None,
            }
        }
        self.base.next()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        add_hints(self.base.size_hint(), self.delta.as_ref().map(A::size_hint))
    }

    #[inline(always)]
    fn fold<Acc, F>(self, init: Acc, mut f: F) -> Acc
    where
        F: FnMut(Acc, Self::Item) -> Acc,
    {
        let acc = match self.delta {
            Some(delta) => delta.fold(init, &mut f),
            None => init,
        };
        self.base.fold(acc, f)
    }

    #[inline(always)]
    fn count(self) -> usize {
        self.delta.map_or(0, Iterator::count) + self.base.count()
    }
}

/// The size hint of two parts, the second optional.
#[inline]
fn add_hints(
    (low, high): (usize, Option<usize>),
    other: Option<(usize, Option<usize>)>,
) -> (usize, Option<usize>) {
    match other {
        None => (low, high),
        Some((other_low, other_high)) => (
            low.saturating_add(other_low),
            high.zip(other_high).and_then(|(a, b)| a.checked_add(b)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{BaseThenDelta, DeltaThenBase};

    #[test]
    fn base_then_delta_lists_the_base_first() {
        let both = BaseThenDelta::new(1..3u32, Some(7..9u32));
        assert_eq!(both.size_hint(), (4, Some(4)));
        assert_eq!(both.collect::<Vec<_>>(), [1, 2, 7, 8]);
        let base_only = BaseThenDelta::new(1..3u32, None::<std::ops::Range<u32>>);
        assert_eq!(base_only.size_hint(), (2, Some(2)));
        #[allow(clippy::unnecessary_fold)] // tests the custom `fold`
        let total = base_only.fold(0, |acc, x| acc + x);
        assert_eq!(total, 3);
        let mut both = BaseThenDelta::new(1..2u32, Some(5..6u32));
        assert_eq!(
            (both.next(), both.next(), both.next(), both.next()),
            (Some(1), Some(5), None, None)
        );
        assert_eq!(BaseThenDelta::new(0..2u32, Some(0..3u32)).count(), 5);
    }

    #[test]
    fn delta_then_base_lists_the_delta_first() {
        let both = DeltaThenBase::new(Some(7..9u32), 1..3u32);
        assert_eq!(both.size_hint(), (4, Some(4)));
        assert_eq!(both.collect::<Vec<_>>(), [7, 8, 1, 2]);
        let base_only = DeltaThenBase::new(None::<std::ops::Range<u32>>, 1..3u32);
        assert_eq!(base_only.collect::<Vec<_>>(), [1, 2]);
        let both = DeltaThenBase::new(Some(7..9u32), 1..3u32);
        let folded = both.fold(Vec::new(), |mut all, x| {
            all.push(x);
            all
        });
        assert_eq!(folded, [7, 8, 1, 2]);
    }
}
