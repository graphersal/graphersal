//! Similarity and distance kernels over the raw component slices of two vectors (book page
//! "Vectors", section "Similarity").
//!
//! Every kernel reads the stored components directly (f32 as is, f16 dequantized lane by lane,
//! i8 as `code * scale` in f64 with the scale applied once per sum where it factors out, two i8
//! vectors in exact integers, bits by popcount) and accumulates in f64, so a score never
//! overflows and never is NaN
//! (components are finite by construction). The loops keep eight independent accumulators per
//! sum over fixed-size chunks: the order of the additions is fixed (the result is deterministic)
//! and LLVM vectorizes the lanes without intrinsics.
//!
//! | Metric | Score | Best |
//! |---|---|---|
//! | cosine | `dot / (\|a\| \|b\|)`, clamped to -1..=1; `0.0` when either vector is zero | highest |
//! | dot | the dot product | highest |
//! | l2 | the Euclidean distance | smallest |
//! | hamming | the number of differing bits (bit vectors only) | smallest |
//!
//! Vectors of different element kinds are compared through their dequantized values; a bit
//! vector compares only with another bit vector (its components are `±1.0`, so the dot product,
//! the cosine and the distance follow from the Hamming distance exactly).

use super::{Data, Vector, VectorElement, VectorError, VectorMetric};

/// The number of independent accumulators per sum.
const LANES: usize = 8;

/// One side of a comparison: a slice of stored components, how to read one as an f64, and the
/// factor of the whole vector (the i8 scale; 1.0 otherwise) that the value is multiplied by.
/// Cosine and dot apply the factor to the sums once, l2 to every value.
trait Lanes: Copy {
    type Item: Copy;
    fn items(&self) -> &[Self::Item];
    fn value(&self, item: Self::Item) -> f64;
    #[inline(always)]
    fn scale(&self) -> f64 {
        1.0
    }
}

/// The f32 value of finite half bits, without branches (so the loop vectorizes): the exponent
/// and mantissa shifted into an f32 and rescaled by 2^112, which is exact for normal and
/// subnormal halves alike. A vector never holds a half infinity or NaN.
#[inline(always)]
fn finite_half(half: u16) -> f32 {
    let magnitude = f32::from_bits(u32::from(half & 0x7fff) << 13) * f32::from_bits(0x7780_0000);
    f32::from_bits(magnitude.to_bits() | (u32::from(half & 0x8000) << 16))
}

#[derive(Clone, Copy)]
struct F32Lanes<'a>(&'a [f32]);

impl Lanes for F32Lanes<'_> {
    type Item = f32;
    #[inline(always)]
    fn items(&self) -> &[f32] {
        self.0
    }
    #[inline(always)]
    fn value(&self, item: f32) -> f64 {
        f64::from(item)
    }
}

#[derive(Clone, Copy)]
struct F16Lanes<'a>(&'a [u16]);

impl Lanes for F16Lanes<'_> {
    type Item = u16;
    #[inline(always)]
    fn items(&self) -> &[u16] {
        self.0
    }
    #[inline(always)]
    fn value(&self, item: u16) -> f64 {
        f64::from(finite_half(item))
    }
}

/// The codes of an i8 vector with its scale: a value is `code * scale`, computed in f64.
#[derive(Clone, Copy)]
struct I8Lanes<'a>(&'a [i8], f32);

impl Lanes for I8Lanes<'_> {
    type Item = i8;
    #[inline(always)]
    fn items(&self) -> &[i8] {
        self.0
    }
    #[inline(always)]
    fn value(&self, item: i8) -> f64 {
        f64::from(item)
    }
    #[inline(always)]
    fn scale(&self) -> f64 {
        f64::from(self.1)
    }
}

/// The sums one pass over two vectors yields.
#[derive(Clone, Copy, Default)]
struct Sums {
    dot: f64,
    left: f64,
    right: f64,
    squared_distance: f64,
}

#[inline(always)]
fn reduce(acc: [f64; LANES]) -> f64 {
    // A fixed pairwise order: the same inputs always give the same sum.
    ((acc[0] + acc[1]) + (acc[2] + acc[3])) + ((acc[4] + acc[5]) + (acc[6] + acc[7]))
}

/// The dot product.
#[inline(always)]
fn dot_kernel<A: Lanes, B: Lanes>(a: A, b: B) -> f64 {
    let (a_chunks, a_rest) = a.items().as_chunks::<LANES>();
    let (b_chunks, b_rest) = b.items().as_chunks::<LANES>();
    let mut acc = [0.0f64; LANES];
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for lane in 0..LANES {
            acc[lane] += a.value(x[lane]) * b.value(y[lane]);
        }
    }
    let mut sum = reduce(acc);
    for (x, y) in a_rest.iter().zip(b_rest) {
        sum += a.value(*x) * b.value(*y);
    }
    sum
}

/// The squared norm of the values (without the scale). Its additions run in exactly the order
/// [`cosine_kernel`] sums the first side's squares, so a norm computed once for a query equals
/// the one a pass would compute.
#[inline(always)]
fn norm_kernel<A: Lanes>(a: A) -> f64 {
    let (chunks, rest) = a.items().as_chunks::<LANES>();
    let mut acc = [0.0f64; LANES];
    for x in chunks {
        for lane in 0..LANES {
            let p = a.value(x[lane]);
            acc[lane] += p * p;
        }
    }
    let mut sum = reduce(acc);
    for x in rest {
        let p = a.value(*x);
        sum += p * p;
    }
    sum
}

/// The dot product and the first side's squared norm, in one pass (values without the scale).
#[inline(always)]
fn cosine_kernel<A: Lanes, B: Lanes>(a: A, b: B) -> Sums {
    let (a_chunks, a_rest) = a.items().as_chunks::<LANES>();
    let (b_chunks, b_rest) = b.items().as_chunks::<LANES>();
    let mut dot = [0.0f64; LANES];
    let mut left = [0.0f64; LANES];
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for lane in 0..LANES {
            let p = a.value(x[lane]);
            let q = b.value(y[lane]);
            dot[lane] += p * q;
            left[lane] += p * p;
        }
    }
    let mut sums = Sums {
        dot: reduce(dot),
        left: reduce(left),
        right: 0.0,
        squared_distance: 0.0,
    };
    for (x, y) in a_rest.iter().zip(b_rest) {
        let p = a.value(*x);
        let q = b.value(*y);
        sums.dot += p * q;
        sums.left += p * p;
    }
    sums
}

/// The squared Euclidean distance.
#[inline(always)]
fn l2_kernel<A: Lanes, B: Lanes>(a: A, b: B) -> f64 {
    let (a_chunks, a_rest) = a.items().as_chunks::<LANES>();
    let (b_chunks, b_rest) = b.items().as_chunks::<LANES>();
    let (sa, sb) = (a.scale(), b.scale());
    let mut acc = [0.0f64; LANES];
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for lane in 0..LANES {
            let d = a.value(x[lane]) * sa - b.value(y[lane]) * sb;
            acc[lane] += d * d;
        }
    }
    let mut sum = reduce(acc);
    for (x, y) in a_rest.iter().zip(b_rest) {
        let d = a.value(*x) * sa - b.value(*y) * sb;
        sum += d * d;
    }
    sum
}

/// The dot product of two i8 code slices, exact in integers.
#[inline(always)]
fn code_dot(a: &[i8], b: &[i8]) -> i64 {
    let (a_chunks, a_rest) = a.as_chunks::<LANES>();
    let (b_chunks, b_rest) = b.as_chunks::<LANES>();
    let mut acc = [0i32; LANES];
    // 8 lanes of at most 128 * 128 per step: an i32 lane holds 2^31 / 2^14 = 131072 steps, far
    // above the largest dimension divided by 8; the total goes to an i64.
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for lane in 0..LANES {
            acc[lane] += i32::from(x[lane]) * i32::from(y[lane]);
        }
    }
    let mut sum: i64 = acc.iter().map(|lane| i64::from(*lane)).sum();
    for (x, y) in a_rest.iter().zip(b_rest) {
        sum += i64::from(*x) * i64::from(*y);
    }
    sum
}

/// The number of differing bits of two packed bit slices (padding bits are 0 in both).
#[inline(always)]
fn hamming_bits(a: &[u8], b: &[u8]) -> u64 {
    let (a_chunks, a_rest) = a.as_chunks::<8>();
    let (b_chunks, b_rest) = b.as_chunks::<8>();
    let mut count = 0u64;
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        count += u64::from((u64::from_le_bytes(*x) ^ u64::from_le_bytes(*y)).count_ones());
    }
    for (x, y) in a_rest.iter().zip(b_rest) {
        count += u64::from((x ^ y).count_ones());
    }
    count
}

/// The score of `metric` from the sums of one pass.
fn finish(metric: VectorMetric, sums: Sums) -> f64 {
    match metric {
        VectorMetric::Cosine => cosine_of(sums.dot, sums.left, sums.right),
        VectorMetric::Dot => sums.dot,
        VectorMetric::L2 => sums.squared_distance.sqrt(),
        // Refused before any kernel runs for non-bit vectors.
        VectorMetric::Hamming => 0.0,
    }
}

fn cosine_of(dot: f64, left: f64, right: f64) -> f64 {
    if left == 0.0 || right == 0.0 {
        return 0.0;
    }
    (dot / (left.sqrt() * right.sqrt())).clamp(-1.0, 1.0)
}

/// Runs the kernel of `metric` on two dequantizing sides.
#[inline(always)]
fn run<A: Lanes, B: Lanes>(metric: VectorMetric, a: A, b: B, b_norm: Option<f64>) -> f64 {
    let (sa, sb) = (a.scale(), b.scale());
    let sums = match metric {
        // The factors cancel out of the cosine, except a zero one (a zero vector).
        VectorMetric::Cosine if sa == 0.0 || sb == 0.0 => Sums::default(),
        VectorMetric::Cosine => Sums {
            right: b_norm.unwrap_or_else(|| norm_kernel(b)),
            ..cosine_kernel(a, b)
        },
        VectorMetric::Dot => Sums {
            dot: dot_kernel(a, b) * sa * sb,
            ..Sums::default()
        },
        VectorMetric::L2 => Sums {
            squared_distance: l2_kernel(a, b),
            ..Sums::default()
        },
        VectorMetric::Hamming => Sums::default(),
    };
    finish(metric, sums)
}

/// Dispatches the second side for a fixed first side.
#[inline(always)]
fn with_right<A: Lanes>(metric: VectorMetric, a: A, right: &Data, norm: Option<f64>) -> f64 {
    match right {
        Data::F32(values) => run(metric, a, F32Lanes(values), norm),
        Data::F16(values) => run(metric, a, F16Lanes(values), norm),
        Data::I8 { codes, scale } => run(metric, a, I8Lanes(codes, *scale), norm),
        // Refused before: a bit vector compares only with a bit vector.
        Data::Bit { .. } => 0.0,
    }
}

impl VectorMetric {
    /// Whether a higher score is better (cosine, dot) rather than a smaller one (l2, hamming:
    /// distances).
    pub fn higher_is_better(self) -> bool {
        match self {
            VectorMetric::Cosine | VectorMetric::Dot => true,
            VectorMetric::L2 | VectorMetric::Hamming => false,
        }
    }
}

impl Vector {
    /// Whether two vectors share their data (a clone of the same value): equal without a
    /// comparison of the components.
    pub(crate) fn same_data(&self, other: &Vector) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    /// The score of `metric` between this vector and `other` (module table: cosine and dot are
    /// similarities, l2 and hamming distances). Exact, accumulated in f64; never NaN.
    ///
    /// Vectors of different element kinds are compared through their dequantized values; a bit
    /// vector compares only with another bit vector ([`VectorError::BitMismatch`]), the hamming
    /// metric only bit vectors ([`VectorError::HammingNeedsBits`]); the dimensions must be equal
    /// ([`VectorError::DimensionMismatch`]).
    ///
    /// ```rust
    /// use graphersal::vector::{Vector, VectorMetric};
    ///
    /// let a = Vector::from_f32(vec![1.0, 0.0]).unwrap();
    /// let b = Vector::from_f32(vec![0.0, 2.0]).unwrap();
    /// assert_eq!(a.similarity(&b, VectorMetric::Cosine).unwrap(), 0.0);
    /// assert_eq!(a.similarity(&b, VectorMetric::L2).unwrap(), 5f64.sqrt());
    /// ```
    pub fn similarity(&self, other: &Vector, metric: VectorMetric) -> Result<f64, VectorError> {
        self.similarity_prepared(other, metric, None)
    }

    /// The squared norm of the stored values without the i8 scale, the part of a cosine a
    /// search computes once for its query ([`similarity_prepared`](Self::similarity_prepared));
    /// `None` for a bit vector.
    pub(crate) fn raw_squared_norm(&self) -> Option<f64> {
        match &*self.0 {
            Data::F32(values) => Some(norm_kernel(F32Lanes(values))),
            Data::F16(values) => Some(norm_kernel(F16Lanes(values))),
            Data::I8 { codes, scale } => Some(norm_kernel(I8Lanes(codes, *scale))),
            Data::Bit { .. } => None,
        }
    }

    /// [`similarity`](Self::similarity) with the query's
    /// [`raw_squared_norm`](Self::raw_squared_norm) computed before (the same value, bit for bit).
    pub(crate) fn similarity_prepared(
        &self,
        other: &Vector,
        metric: VectorMetric,
        other_norm: Option<f64>,
    ) -> Result<f64, VectorError> {
        let (left, right) = (self.dimension(), other.dimension());
        if left != right {
            return Err(VectorError::DimensionMismatch { left, right });
        }
        match (&*self.0, &*other.0) {
            (Data::Bit { bits: a, dimension }, Data::Bit { bits: b, .. }) => {
                let differing = hamming_bits(a, b) as f64;
                let dimension = f64::from(*dimension);
                Ok(match metric {
                    VectorMetric::Hamming => differing,
                    // Components are ±1: a·b = d - 2h, |a|² = |b|² = d, |a - b|² = 4h.
                    VectorMetric::Dot => dimension - 2.0 * differing,
                    VectorMetric::Cosine => {
                        ((dimension - 2.0 * differing) / dimension).clamp(-1.0, 1.0)
                    }
                    VectorMetric::L2 => 2.0 * differing.sqrt(),
                })
            }
            (Data::Bit { .. }, other) | (other, Data::Bit { .. }) => {
                Err(VectorError::BitMismatch {
                    element: element_of(other),
                })
            }
            (a, _) if metric == VectorMetric::Hamming => Err(VectorError::HammingNeedsBits {
                element: element_of(a),
            }),
            (
                Data::I8 {
                    codes: a,
                    scale: sa,
                },
                Data::I8 {
                    codes: b,
                    scale: sb,
                },
            ) if metric != VectorMetric::L2 => {
                // Exact integer dot product; the scales factor out.
                let dot = code_dot(a, b) as f64;
                Ok(match metric {
                    VectorMetric::Cosine => {
                        if *sa == 0.0 || *sb == 0.0 {
                            0.0
                        } else {
                            let left = code_dot(a, a) as f64;
                            let right = code_dot(b, b) as f64;
                            cosine_of(dot, left, right)
                        }
                    }
                    _ => dot * f64::from(*sa) * f64::from(*sb),
                })
            }
            (Data::F32(a), right) => Ok(with_right(metric, F32Lanes(a), right, other_norm)),
            (Data::F16(a), right) => Ok(with_right(metric, F16Lanes(a), right, other_norm)),
            (Data::I8 { codes, scale }, right) => Ok(with_right(
                metric,
                I8Lanes(codes, *scale),
                right,
                other_norm,
            )),
        }
    }
}

fn element_of(data: &Data) -> VectorElement {
    match data {
        Data::F32(_) => VectorElement::F32,
        Data::F16(_) => VectorElement::F16,
        Data::I8 { .. } => VectorElement::I8,
        Data::Bit { .. } => VectorElement::Bit,
    }
}

/// The component-wise mean of vectors (each counted `weight` times), as an f32 vector; `None`
/// when no weight was added. The sums are f64.
pub(crate) struct MeanAccumulator {
    sums: Vec<f64>,
    count: u64,
}

impl MeanAccumulator {
    pub(crate) fn new() -> Self {
        MeanAccumulator {
            sums: Vec::new(),
            count: 0,
        }
    }

    /// Adds `vector` `weight` times; a dimension other than the first vector's is an error.
    pub(crate) fn add(&mut self, vector: &Vector, weight: u64) -> Result<(), VectorError> {
        if weight == 0 {
            return Ok(());
        }
        if self.count == 0 && self.sums.is_empty() {
            self.sums = vec![0.0; vector.dimension()];
        } else if self.sums.len() != vector.dimension() {
            return Err(VectorError::DimensionMismatch {
                left: self.sums.len(),
                right: vector.dimension(),
            });
        }
        let factor = weight as f64;
        for (sum, value) in self.sums.iter_mut().zip(vector.iter_f32()) {
            *sum += f64::from(value) * factor;
        }
        self.count = self.count.saturating_add(weight);
        Ok(())
    }

    /// The mean, or `None` without any vector.
    pub(crate) fn finish(self) -> Result<Option<Vector>, VectorError> {
        if self.count == 0 {
            return Ok(None);
        }
        let count = self.count as f64;
        let mean: Vec<f32> = self.sums.iter().map(|sum| (sum / count) as f32).collect();
        Vector::from_f32(mean).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::super::half::{f16_is_finite, f16_to_f32};
    use super::finite_half;

    #[test]
    fn the_branch_free_half_conversion_is_exact_for_every_finite_half() {
        for half in 0..=u16::MAX {
            if f16_is_finite(half) {
                assert_eq!(
                    finite_half(half).to_bits(),
                    f16_to_f32(half).to_bits(),
                    "{half:#06x}"
                );
            }
        }
    }
}
