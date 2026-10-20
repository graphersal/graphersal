//! Up-front capacities whose size comes from query data (a bulk, a row count, a list length, a
//! cross product). Such a number is untrusted: `Vec::with_capacity(n)` with a huge `n` aborts the
//! process ("memory allocation of N bytes failed") before any limit can report an error. Every
//! reservation fed by query data goes through this module instead:
//!
//! - [`capped`] / [`vec_capped`]: a hint only. The up-front capacity is at most
//!   [`MAX_UPFRONT_CAPACITY`] elements; the vector still grows normally past it, so a real
//!   stream of that size pays a few doublings and nothing else.
//! - [`try_reserve_exact`]: an exact reservation a caller genuinely needs (it checked the size
//!   against its limits first). A failed reservation is a
//!   [`TraverserError::ResourceLimitExceeded`] of [`ALLOCATION_LIMIT`] with a `help()` text,
//!   never an abort.

use crate::*;

/// The largest capacity reserved up front from an untrusted count, in elements.
pub(crate) const MAX_UPFRONT_CAPACITY: usize = 4096;

/// The `limit` of the [`TraverserError::ResourceLimitExceeded`] a failed exact reservation
/// reports: the system could not provide the memory (not an option a query can raise).
pub(crate) const ALLOCATION_LIMIT: &str = "memory.allocation";

/// `n` as an up-front capacity: at most [`MAX_UPFRONT_CAPACITY`].
#[inline]
pub(crate) fn capped(n: usize) -> usize {
    n.min(MAX_UPFRONT_CAPACITY)
}

/// A `u64` count (a bulk) as an up-front capacity: at most [`MAX_UPFRONT_CAPACITY`].
#[inline]
pub(crate) fn capped_u64(n: u64) -> usize {
    usize::try_from(n).map_or(MAX_UPFRONT_CAPACITY, capped)
}

/// An empty vector with room for `n` elements, at most [`MAX_UPFRONT_CAPACITY`] of them.
#[inline]
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
}
