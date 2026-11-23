//! Up-front capacities whose size comes from query data (a bulk, a row count, a list length, a
//! cross product). Such a number is untrusted: `Vec::with_capacity(n)` with a huge `n` aborts the
//! process ("memory allocation of N bytes failed") before any limit can report an error. Every
//! reservation fed by query data goes through this module instead:
//!
//! - `capped` / `vec_capped` (GQL steps): a hint only. The up-front capacity is at most
//!   `MAX_UPFRONT_CAPACITY` elements; the vector still grows normally past it, so a real
//!   stream of that size pays a few doublings and nothing else.
//! - [`try_reserve_exact`]: an exact reservation a caller genuinely needs (it checked the size
//!   against its limits first). A failed reservation is a
//!   [`TraverserError::ResourceLimitExceeded`] of [`ALLOCATION_LIMIT`] with a `help()` text,
//!   never an abort.
//!
//! [`reserve_out_of_place`] is the growth of a traverser buffer that a stream fills (a flat map's
//! output, `repeat()`'s emitted traversers): large buffers never grow by `realloc`.

use crate::*;

/// The largest capacity reserved up front from an untrusted count, in elements.
#[cfg(any(feature = "gql", test))]
pub(crate) const MAX_UPFRONT_CAPACITY: usize = 4096;

/// The `limit` of the [`TraverserError::ResourceLimitExceeded`] a failed exact reservation
/// reports: the system could not provide the memory (not an option a query can raise).
pub(crate) const ALLOCATION_LIMIT: &str = "memory.allocation";

/// `n` as an up-front capacity: at most [`MAX_UPFRONT_CAPACITY`].
#[inline]
#[cfg(any(feature = "gql", test))]
pub(crate) fn capped(n: usize) -> usize {
    n.min(MAX_UPFRONT_CAPACITY)
}

/// A `u64` count (a bulk) as an up-front capacity: at most [`MAX_UPFRONT_CAPACITY`].
#[cfg(any(feature = "gql", test))]
#[inline]
pub(crate) fn capped_u64(n: u64) -> usize {
    usize::try_from(n).map_or(MAX_UPFRONT_CAPACITY, capped)
}

/// An empty vector with room for `n` elements, at most [`MAX_UPFRONT_CAPACITY`] of them.
#[inline]
#[cfg(any(feature = "gql", test))]
pub(crate) fn vec_capped<T>(n: usize) -> Vec<T> {
    Vec::with_capacity(capped(n))
}

/// Reserves room for exactly `additional` more elements in `items`, or fails with
/// [`TraverserError::ResourceLimitExceeded`] of [`ALLOCATION_LIMIT`] (`observed`: the elements
/// asked for) when the allocator cannot provide it.
pub(crate) fn try_reserve_exact<T>(items: &mut Vec<T>, additional: usize) -> TraverserResult<()> {
    items
        .try_reserve_exact(additional)
        .map_err(|_| allocation_error(additional))
}

/// The error of a reservation of `elements` elements the allocator refused.
#[cold]
pub(crate) fn allocation_error(elements: usize) -> Box<TraverserError> {
    Box::new(TraverserError::ResourceLimitExceeded {
        limit: ALLOCATION_LIMIT.to_string(),
        max: None,
        observed: Some(elements as u64),
    })
}

/// From this many bytes of capacity on, a traverser buffer grows by a fresh allocation and a move
/// instead of `realloc` (see [`reserve_out_of_place`]). It is the smallest size the macOS
/// allocator may serve as a large block (on machines without its medium zone); below it the
/// pushes grow the buffer by `realloc` (cheap, and harmless).
const OUT_OF_PLACE_GROWTH_BYTES: usize = 128 * 1024;

/// Makes room for `additional` more items in `buffer`, which held `base` items when the current
/// stream of `total` inputs began and which `done` of those inputs have filled since. A large
/// buffer grows by allocating a new block and moving the items over, never by `realloc`: the
/// macOS allocator (libmalloc, measured on macOS 27) never reuses nor returns a large block that
/// `realloc` grew in place once it is freed, so a query whose output buffer grew that way kept
/// its whole size after every execution (+288 MB per run of a two-hop query over 400k edges,
/// test `query_memory_tests::repeated_runs_keep_the_process_footprint_flat`). A fresh block is
/// freed normally. The new block holds what the inputs so far project for all inputs plus
/// `additional` (at least twice the capacity, at most four times), so a uniform stream reaches
/// its final size in few moves and without the doubling's overshoot; the moves copy less than
/// the final size in total, and the old block lives only during its move. Call it before an
/// input, with the most items one input may add, so the pushes never grow a large buffer.
#[inline]
pub(crate) fn reserve_out_of_place<T>(
    buffer: &mut Vec<T>,
    additional: usize,
    base: usize,
    done: usize,
    total: usize,
) {
    if buffer.capacity().saturating_mul(size_of::<T>()) < OUT_OF_PLACE_GROWTH_BYTES
        || buffer.capacity() - buffer.len() >= additional
    {
        // A small buffer is left to the pushes, which grow it as any `Vec` (and exactly as
        // before, so the estimated memory figures of small queries do not move).
        return;
    }
    grow_out_of_place(buffer, additional, base, done, total);
}

#[cold]
#[inline(never)]
fn grow_out_of_place<T>(
    buffer: &mut Vec<T>,
    additional: usize,
    base: usize,
    done: usize,
    total: usize,
) {
    let capacity = buffer.capacity();
    let needed = buffer.len().saturating_add(additional);
    // What the inputs so far project for all inputs, plus the room asked for the next one.
    let added = buffer.len().saturating_sub(base);
    let projected = (added as u128 * total as u128)
        .checked_div(done as u128)
        .map_or(0, |all| usize::try_from(all).unwrap_or(usize::MAX))
        .saturating_add(base)
        .saturating_add(additional);
    let wanted = projected
        .max(capacity.saturating_mul(2))
        .min(capacity.saturating_mul(4))
        .max(needed);
    let mut grown = Vec::with_capacity(wanted);
    grown.append(buffer);
    *buffer = grown;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacities_from_untrusted_counts_are_capped() {
        assert_eq!(capped(10), 10);
        assert_eq!(capped(usize::MAX), MAX_UPFRONT_CAPACITY);
        assert_eq!(capped_u64(u64::MAX), MAX_UPFRONT_CAPACITY);
        assert_eq!(capped_u64(3), 3);
        let items: Vec<u64> = vec_capped(10_650_053_687_364);
        assert_eq!(items.capacity(), MAX_UPFRONT_CAPACITY);
    }

    #[test]
    fn a_refused_exact_reservation_is_an_error_not_an_abort() {
        let mut items: Vec<u64> = Vec::new();
        let Err(error) = try_reserve_exact(&mut items, usize::MAX / 2) else {
            panic!("an impossible reservation succeeded");
        };
        assert!(matches!(
            *error,
            TraverserError::ResourceLimitExceeded { ref limit, .. } if limit == ALLOCATION_LIMIT
        ));
        assert!(try_reserve_exact(&mut items, 16).is_ok());
    }

    /// A full `Vec<u64>` of `len` items at exactly that capacity.
    fn full(len: usize) -> Vec<u64> {
        let mut buffer = Vec::with_capacity(len);
        buffer.extend(0..len as u64);
        buffer.shrink_to_fit();
        assert_eq!(buffer.capacity(), len);
        buffer
    }

    #[test]
    fn room_already_there_changes_nothing() {
        let mut buffer = full(10);
        buffer.reserve_exact(5);
        let capacity = buffer.capacity();
        reserve_out_of_place(&mut buffer, 5, 0, 1, 2);
        assert_eq!(buffer.capacity(), capacity);
    }

    #[test]
    fn a_small_buffer_is_left_to_the_pushes() {
        let mut buffer = full(100);
        reserve_out_of_place(&mut buffer, 64, 0, 50, 100);
        assert_eq!(buffer.capacity(), 100);
    }

    #[test]
    fn a_large_buffer_grows_to_the_projection_within_two_to_four_times() {
        let large = OUT_OF_PLACE_GROWTH_BYTES / size_of::<u64>() + 1;
        // 1 item per input so far, 2.5 x as many inputs in total: the projection.
        let mut buffer = full(2 * large);
        reserve_out_of_place(&mut buffer, 64, 0, 2 * large, 5 * large);
        assert_eq!(buffer.capacity(), 5 * large + 64);
        assert!(buffer.iter().copied().eq(0..2 * large as u64));
        // Items from before the stream are not projected: `large` of them, then 1 per input.
        let mut buffer = full(2 * large);
        reserve_out_of_place(&mut buffer, 64, large, large, 4 * large);
        assert_eq!(buffer.capacity(), 5 * large + 64);
        // A projection of 10 x is held at 4 x the capacity.
        let mut buffer = full(large);
        reserve_out_of_place(&mut buffer, 64, 0, large / 10, large);
        assert_eq!(buffer.capacity(), 4 * large);
        // No projection yet (or a smaller one): twice the capacity.
        let mut buffer = full(large);
        reserve_out_of_place(&mut buffer, 64, 0, 0, 0);
        assert_eq!(buffer.capacity(), 2 * large);
        // More asked than four times the capacity: what is asked.
        let mut buffer = full(large);
        reserve_out_of_place(&mut buffer, 5 * large, 0, 1, 1);
        assert_eq!(buffer.capacity(), 6 * large);
    }
}
