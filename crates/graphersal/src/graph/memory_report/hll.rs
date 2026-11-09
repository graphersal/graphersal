//! A small HyperLogLog (Flajolet et al. 2007) for the memory report's distinct string counts:
//! 256 one-byte registers (`p = 8`), a standard error of about `1.04 / sqrt(256)` = 6.5 %, with
//! the linear-counting correction for small cardinalities (exact enough below ~200 values).
//!
//! The sketch starts SPARSE: a sorted, deduplicated list of the full 64-bit hashes, at most
//! [`SPARSE_MAX`] (32) of them, so that it never takes more heap than the dense registers
//! (32 x 8 = 256 bytes; an empty sketch allocates nothing). While sparse, the estimate is the
//! exact number of distinct hashes (exact up to a 64-bit hash collision). The 33rd distinct hash
//! turns it dense: every kept hash goes into the registers, so the dense state is the same as if
//! the sketch had been dense from the start. The memory report keeps one sketch per
//! `(kind, label, key)` row holding strings; most rows see few distinct texts, so the sparse form
//! saves the 256 bytes per row a dense sketch took (about 85 MB on the `large` sample graph).
//!
//! The hash is FNV-1a over the bytes, finished with the `splitmix64` mixer so that the register
//! index (top 8 bits) and the rank (the rest) are well spread even for short, similar texts.
//! Deterministic: the same set of texts always gives the same estimate, in any order.

/// The number of index bits.
const P: u32 = 8;
/// The number of registers.
const M: usize = 1 << P;
/// The most hashes the sparse form keeps: as many bytes as the dense registers.
const SPARSE_MAX: usize = M / std::mem::size_of::<u64>();

/// A HyperLogLog sketch of 256 registers, sparse while it saw at most [`SPARSE_MAX`] distinct
/// hashes.
#[derive(Clone)]
pub(crate) struct HyperLogLog {
    state: State,
}

/// The two representations of a [`HyperLogLog`].
#[derive(Clone)]
enum State {
    /// The distinct hashes seen, sorted ascending, at most [`SPARSE_MAX`].
    Sparse(Vec<u64>),
    /// The registers.
    Dense(Box<[u8; M]>),
}

impl Default for HyperLogLog {
    fn default() -> Self {
        HyperLogLog {
            state: State::Sparse(Vec::new()),
        }
    }
}

impl std::fmt::Debug for HyperLogLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let form = match self.state {
            State::Sparse(_) => "sparse",
            State::Dense(_) => "dense",
        };
        write!(f, "HyperLogLog({form}, ~{})", self.estimate())
    }
}

/// FNV-1a over `bytes`, then the `splitmix64` finalizer.
fn hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

/// Puts the hash `h` into the dense `registers`.
fn add_to_registers(registers: &mut [u8; M], h: u64) {
    let index = (h >> (64 - P)) as usize;
    // The rank of the remaining 56 bits: leading zeros + 1, at most 57.
    let rest = h << P;
    let rank = (rest.leading_zeros().min(64 - P) + 1) as u8;
    if let Some(register) = registers.get_mut(index)
        && rank > *register
    {
        *register = rank;
    }
}

impl HyperLogLog {
    /// Adds one text.
    pub(crate) fn add(&mut self, text: &[u8]) {
        let h = hash(text);
        match &mut self.state {
            State::Dense(registers) => add_to_registers(registers, h),
            State::Sparse(hashes) => {
                let Err(at) = hashes.binary_search(&h) else {
                    return;
                };
                if hashes.len() < SPARSE_MAX {
                    hashes.insert(at, h);
                    return;
                }
                let mut registers = Box::new([0u8; M]);
                for &kept in hashes.iter() {
                    add_to_registers(&mut registers, kept);
                }
                add_to_registers(&mut registers, h);
                self.state = State::Dense(registers);
            }
        }
    }

    /// The estimated number of distinct texts added.
    pub(crate) fn estimate(&self) -> u64 {
        let registers = match &self.state {
            State::Sparse(hashes) => return hashes.len() as u64,
            State::Dense(registers) => registers,
        };
        let m = M as f64;
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let mut sum = 0.0f64;
        let mut zeros = 0usize;
        for &register in registers.iter() {
            sum += 2f64.powi(-i32::from(register));
            if register == 0 {
                zeros += 1;
            }
        }
        let raw = alpha * m * m / sum;
        let estimate = if raw <= 2.5 * m && zeros > 0 {
            // Linear counting for small cardinalities.
            m * (m / zeros as f64).ln()
        } else {
            raw
        };
        estimate.round() as u64
    }

    /// Whether the sketch is still sparse (tests).
    #[cfg(test)]
    fn is_sparse(&self) -> bool {
        matches!(self.state, State::Sparse(_))
    }

    /// The heap bytes the sketch holds (tests).
    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        match &self.state {
            State::Sparse(hashes) => hashes.capacity() * std::mem::size_of::<u64>(),
            State::Dense(_) => M,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HyperLogLog, M, SPARSE_MAX, add_to_registers, hash};

    fn sketch_of(n: usize) -> HyperLogLog {
        let mut sketch = HyperLogLog::default();
        for i in 0..n {
            sketch.add(format!("value-{i}").as_bytes());
        }
        sketch
    }

    fn estimate_of(n: usize) -> u64 {
        sketch_of(n).estimate()
    }

    /// The estimate of a sketch that was dense from the first text.
    fn dense_estimate_of(n: usize) -> u64 {
        let mut registers = Box::new([0u8; M]);
        for i in 0..n {
            add_to_registers(&mut registers, hash(format!("value-{i}").as_bytes()));
        }
        HyperLogLog {
            state: super::State::Dense(registers),
        }
        .estimate()
    }

    #[test]
    fn empty_and_repeated_values() {
        assert_eq!(HyperLogLog::default().estimate(), 0);
        assert_eq!(HyperLogLog::default().heap_bytes(), 0);
        let mut sketch = HyperLogLog::default();
        for _ in 0..1000 {
            sketch.add(b"same");
        }
        assert_eq!(sketch.estimate(), 1);
        assert!(sketch.is_sparse());
    }

    #[test]
    fn estimates_are_within_the_expected_error() {
        for n in [10usize, 100, 1_000, 10_000, 100_000] {
            let estimate = estimate_of(n) as f64;
            let error = (estimate - n as f64).abs() / n as f64;
            // Three standard errors (6.5 % each) at most.
            assert!(error < 0.2, "n = {n}: estimate {estimate}, error {error}");
        }
    }

    #[test]
    fn small_sets_are_counted_exactly_and_stay_sparse() {
        for n in 0..=SPARSE_MAX {
            let mut sketch = sketch_of(n);
            // Repeats change nothing.
            for i in 0..n {
                sketch.add(format!("value-{i}").as_bytes());
            }
            assert!(sketch.is_sparse(), "n = {n}");
            assert_eq!(sketch.estimate(), n as u64, "n = {n}");
            assert!(
                sketch.heap_bytes() <= M,
                "n = {n}: {} bytes",
                sketch.heap_bytes()
            );
        }
    }

    #[test]
    fn the_next_distinct_text_turns_it_dense_like_a_dense_sketch() {
        let mut sketch = sketch_of(SPARSE_MAX);
        assert!(sketch.is_sparse());
        // A repeat at the threshold keeps it sparse.
        sketch.add(b"value-0");
        assert!(sketch.is_sparse());
        sketch.add(format!("value-{SPARSE_MAX}").as_bytes());
        assert!(!sketch.is_sparse());
        assert_eq!(sketch.heap_bytes(), M);
        for n in [SPARSE_MAX + 1, SPARSE_MAX + 2, 100, 1_000] {
            assert_eq!(estimate_of(n), dense_estimate_of(n), "n = {n}");
        }
    }

    #[test]
    fn the_estimate_is_continuous_around_the_threshold() {
        let mut previous = estimate_of(SPARSE_MAX - 8);
        for n in SPARSE_MAX - 7..=SPARSE_MAX + 16 {
            let estimate = estimate_of(n);
            let step = estimate.abs_diff(previous);
            assert!(step <= 3, "n = {n}: {previous} -> {estimate}");
            assert!(
                estimate.abs_diff(n as u64) <= 4,
                "n = {n}: estimate {estimate}"
            );
            previous = estimate;
        }
    }

    #[test]
    fn the_estimate_does_not_depend_on_the_order() {
        for n in [5usize, SPARSE_MAX, SPARSE_MAX + 1, 300] {
            let mut reversed = HyperLogLog::default();
            for i in (0..n).rev() {
                reversed.add(format!("value-{i}").as_bytes());
            }
            let forward = sketch_of(n);
            assert_eq!(reversed.is_sparse(), forward.is_sparse(), "n = {n}");
            assert_eq!(reversed.estimate(), forward.estimate(), "n = {n}");
            assert_eq!(sketch_of(n).estimate(), forward.estimate(), "n = {n}");
        }
    }
}
