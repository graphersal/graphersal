//! A small HyperLogLog (Flajolet et al. 2007) for the memory report's distinct string counts:
//! 256 one-byte registers (`p = 8`), a standard error of about `1.04 / sqrt(256)` = 6.5 %, with
//! the linear-counting correction for small cardinalities (exact enough below ~200 values).
//!
//! The hash is FNV-1a over the bytes, finished with the `splitmix64` mixer so that the register
//! index (top 8 bits) and the rank (the rest) are well spread even for short, similar texts.
//! Deterministic: the same texts always give the same estimate.

/// The number of index bits.
const P: u32 = 8;
/// The number of registers.
const M: usize = 1 << P;

/// A HyperLogLog sketch of 256 registers.
#[derive(Clone)]
pub(crate) struct HyperLogLog {
    registers: Box<[u8; M]>,
}

impl Default for HyperLogLog {
    fn default() -> Self {
        HyperLogLog {
            registers: Box::new([0; M]),
        }
    }
}

impl std::fmt::Debug for HyperLogLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HyperLogLog(~{})", self.estimate())
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

impl HyperLogLog {
    /// Adds one text.
    pub(crate) fn add(&mut self, text: &[u8]) {
        let h = hash(text);
        let index = (h >> (64 - P)) as usize;
        // The rank of the remaining 56 bits: leading zeros + 1, at most 57.
        let rest = h << P;
        let rank = (rest.leading_zeros().min(64 - P) + 1) as u8;
        if let Some(register) = self.registers.get_mut(index)
            && rank > *register
        {
            *register = rank;
        }
    }

    /// The estimated number of distinct texts added.
    pub(crate) fn estimate(&self) -> u64 {
        let m = M as f64;
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let mut sum = 0.0f64;
        let mut zeros = 0usize;
        for &register in self.registers.iter() {
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
}

#[cfg(test)]
mod tests {
    use super::HyperLogLog;

    fn estimate_of(n: usize) -> u64 {
        let mut sketch = HyperLogLog::default();
        for i in 0..n {
            sketch.add(format!("value-{i}").as_bytes());
        }
        sketch.estimate()
    }

    #[test]
    fn empty_and_repeated_values() {
        assert_eq!(HyperLogLog::default().estimate(), 0);
        let mut sketch = HyperLogLog::default();
        for _ in 0..1000 {
            sketch.add(b"same");
        }
        assert_eq!(sketch.estimate(), 1);
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
}
