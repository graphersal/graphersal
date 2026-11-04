//! Vectors (embeddings): a fixed-length list of numbers stored as one value, in one of four
//! element kinds (book page "Vectors").
//!
//! | Element | Component | Bytes per component | Value of a component |
//! |---|---|---|---|
//! | [`VectorElement::F32`] | IEEE single | 4 | the f32 |
//! | [`VectorElement::F16`] | IEEE half | 2 | the half, exactly as an f32 |
//! | [`VectorElement::I8`] | i8 code + one f32 scale per vector | 1 | `code * scale` |
//! | [`VectorElement::Bit`] | one sign bit | 1/8 | `1.0` for a set bit, `-1.0` otherwise |
//!
//! A vector is immutable and shares its data behind an `Arc` (a clone is a reference count).
//! Construction checks the dimension (1 to [`MAX_VECTOR_DIMENSION`]) and refuses NaN and the
//! infinities, so ordering and similarity are always defined; `-0.0` is kept (and equals `0.0`).
//!
//! **Quantization** ([`Vector::quantize`]) converts between the kinds: f32 -> f16 rounds to
//! nearest even (a component beyond the half range, 65504, is an error, never an infinity);
//! f32 -> i8 is symmetric scalar quantization (`scale = max|x| / 127`, `code = round(x / scale)`
//! in -127..=127); f32 -> bit keeps the sign (`x > 0` sets the bit). Every other conversion goes
//! through the f32 values ([`Vector::to_f32_vec`]).
//!
//! **Equality** is per kind: two vectors are equal when they have the same kind, the same
//! dimension and equal components (f32/f16 by value, so `-0.0 == 0.0`; i8 codes and the scale;
//! bits). **Order** (orderability, a Graphersal extension: TinkerPop has no vector): by kind
//! (f32, f16, i8, bit), then dimension, then the component values lexicographically (an i8
//! vector then by its scale).

pub mod embed;
pub(crate) mod half;
pub(crate) mod similarity;
pub(crate) mod tagged;

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// The largest dimension of a vector (65 536 components).
pub const MAX_VECTOR_DIMENSION: usize = 65_536;

/// The largest code magnitude of [`VectorElement::I8`] (symmetric: -127..=127; a stored -128 is
/// accepted and read as `-128 * scale`).
const I8_MAX_CODE: f32 = 127.0;

/// How a vector stores its components.
///
/// The set is closed for 0.2.0 (every kind is implemented in the value type, the file formats and
/// the conversions), but `#[non_exhaustive]` keeps a later kind a minor change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum VectorElement {
    /// IEEE 754 single precision (4 bytes per component), the kind of a new vector.
    F32,
    /// IEEE 754 half precision (2 bytes per component).
    F16,
    /// Symmetric scalar quantization: one i8 code per component and one f32 scale per vector
    /// (`value = code * scale`).
    I8,
    /// Binary quantization: one sign bit per component (`1.0` or `-1.0`), packed eight to a
    /// byte, the first component in the most significant bit.
    Bit,
}

impl VectorElement {
    /// Every kind, in their order.
    pub const ALL: [VectorElement; 4] = [
        VectorElement::F32,
        VectorElement::F16,
        VectorElement::I8,
        VectorElement::Bit,
    ];

    /// The name used in the DSL, the schema and the file formats: `f32`, `f16`, `i8`, `bit`.
    pub fn name(self) -> &'static str {
        match self {
            VectorElement::F32 => "f32",
            VectorElement::F16 => "f16",
            VectorElement::I8 => "i8",
            VectorElement::Bit => "bit",
        }
    }

    /// The kind of a name (`f32`, `f16`, `i8`, `bit`; exact, lowercase).
    pub fn from_name(name: &str) -> Option<VectorElement> {
        VectorElement::ALL
            .into_iter()
            .find(|kind| kind.name() == name)
    }

    /// The kind byte of the binary format (format spec 3: 0 f32, 1 f16, 2 i8, 3 bit).
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn code(self) -> u8 {
        match self {
            VectorElement::F32 => 0,
            VectorElement::F16 => 1,
            VectorElement::I8 => 2,
            VectorElement::Bit => 3,
        }
    }

    /// The kind of a kind byte.
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn from_code(code: u8) -> Option<VectorElement> {
        VectorElement::ALL
            .into_iter()
            .find(|kind| kind.code() == code)
    }

    /// The bytes `dimension` components take (without the i8 scale).
    pub(crate) fn data_bytes(self, dimension: usize) -> usize {
        match self {
            VectorElement::F32 => dimension * 4,
            VectorElement::F16 => dimension * 2,
            VectorElement::I8 => dimension,
            VectorElement::Bit => dimension.div_ceil(8),
        }
    }
}

impl fmt::Display for VectorElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A similarity or distance measure between two vectors. In 0.2.0 it is the `metric` annotation
/// of a schema's vector declaration; the search steps take it as their metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VectorMetric {
    /// Cosine similarity (the default): the dot product over the product of the norms, `0.0`
    /// for a zero vector.
    Cosine,
    /// The dot product.
    Dot,
    /// The Euclidean distance.
    L2,
    /// The number of differing bits (bit vectors).
    Hamming,
}

impl VectorMetric {
    /// Every metric, in the order the documentation lists them.
    pub const ALL: [VectorMetric; 4] = [
        VectorMetric::Cosine,
        VectorMetric::Dot,
        VectorMetric::L2,
        VectorMetric::Hamming,
    ];

    /// The name: `cosine`, `dot`, `l2`, `hamming`.
    pub fn name(self) -> &'static str {
        match self {
            VectorMetric::Cosine => "cosine",
            VectorMetric::Dot => "dot",
            VectorMetric::L2 => "l2",
            VectorMetric::Hamming => "hamming",
        }
    }

    /// The metric of a name (exact, lowercase).
    pub fn from_name(name: &str) -> Option<VectorMetric> {
        VectorMetric::ALL
            .into_iter()
            .find(|metric| metric.name() == name)
    }
}

impl fmt::Display for VectorMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A vector that cannot be built or converted.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VectorError {
    /// The dimension is 0 or above [`MAX_VECTOR_DIMENSION`].
    #[error("a vector has 1 to 65536 components, not {dimension}")]
    #[non_exhaustive]
    Dimension {
        /// The dimension asked for.
        dimension: usize,
    },
    /// A component is NaN or an infinity (or does not fit the element kind).
    #[error("component {index} of the vector is {value}: vectors hold finite numbers only")]
    #[non_exhaustive]
    NotFinite {
        /// The position of the component.
        index: usize,
        /// The value, as text (`NaN`, `inf`, `1e300`).
        value: String,
    },
    /// A component is not a number.
    #[error("component {index} of the vector is a {found}, not a number")]
    #[non_exhaustive]
    NotANumber {
        /// The position of the component.
        index: usize,
        /// The type name of what was found.
        found: String,
    },
    /// A component of an f32 vector lies beyond the range of the target kind (f16: 65504).
    #[error("component {index} ({value}) is beyond the range of a {element} vector")]
    #[non_exhaustive]
    OutOfRange {
        /// The position of the component.
        index: usize,
        /// The value, as text.
        value: String,
        /// The target kind.
        element: VectorElement,
    },
    /// The i8 scale is negative, NaN or infinite.
    #[error("the scale of an i8 vector is a finite number >= 0, not {scale}")]
    #[non_exhaustive]
    Scale {
        /// The scale, as text.
        scale: String,
    },
    /// Packed bits that do not match the dimension (wrong length or padding bits set).
    #[error("{reason}")]
    #[non_exhaustive]
    Bits {
        /// What is wrong.
        reason: String,
    },
    /// A value that is not a vector (nor a list of numbers) where one is needed.
    #[error("expected a vector, found {found}")]
    #[non_exhaustive]
    NotAVector {
        /// What was found (its type and a preview).
        found: String,
    },
    /// An unknown element kind name.
    #[error("unknown vector element {name}: the kinds are f32, f16, i8 and bit")]
    #[non_exhaustive]
    UnknownElement {
        /// The name (quoted, cut after 64 characters).
        name: String,
    },
    /// Two vectors of different dimensions compared (or averaged).
    #[error("vectors of dimensions {left} and {right} cannot be compared")]
    #[non_exhaustive]
    DimensionMismatch {
        /// The dimension of the first vector (the stored one).
        left: usize,
        /// The dimension of the second vector (the query).
        right: usize,
    },
    /// A bit vector compared with a vector of another element kind.
    #[error("a bit vector compares only with another bit vector, not with a {element} vector")]
    #[non_exhaustive]
    BitMismatch {
        /// The element kind of the other vector.
        element: VectorElement,
    },
    /// The hamming metric asked for vectors that are not bit vectors.
    #[error("the hamming metric compares bit vectors, not {element} vectors")]
    #[non_exhaustive]
    HammingNeedsBits {
        /// The element kind found.
        element: VectorElement,
    },
    /// `nearest()` got different query vectors for different inputs from its query traversal.
    #[error("the query traversal gave different vectors for different inputs")]
    QueryNotConstant,
    /// `nearest()` without a metric over elements whose schema declares different metrics.
    #[error("the schema declares the metric {first} for one element and {second} for another")]
    #[non_exhaustive]
    MetricConflict {
        /// The metric of the first element.
        first: VectorMetric,
        /// The other metric.
        second: VectorMetric,
    },
    /// An unknown metric name.
    #[error("unknown vector metric {name}: the metrics are cosine, dot, l2 and hamming")]
    #[non_exhaustive]
    UnknownMetric {
        /// The name (quoted, cut after 64 characters).
        name: String,
    },
}

impl VectorError {
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn unknown_element(name: &str) -> Self {
        let cut: String = name.chars().take(64).collect();
        VectorError::UnknownElement {
            name: format!("{cut:?}"),
        }
    }

    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn unknown_metric(name: &str) -> Self {
        let cut: String = name.chars().take(64).collect();
        VectorError::UnknownMetric {
            name: format!("{cut:?}"),
        }
    }

    pub(crate) fn not_a_number(index: usize, found: impl Into<String>) -> Self {
        VectorError::NotANumber {
            index,
            found: found.into(),
        }
    }

    /// The help text of this error, for the `help()` of the errors that wrap it.
    pub fn help(&self) -> String {
        match self {
            VectorError::Dimension { .. } => "A vector has between 1 and 65536 components. \
                 Build it from a non-empty list: vector([0.1, 0.2, 0.3])."
                .to_string(),
            VectorError::NotFinite { .. } | VectorError::NotANumber { .. } => {
                "Every component of a vector is a finite number (no NaN, no infinity, no \
                 null). Check the embedding that produced it: vector([0.1, 0.2, 0.3])."
                    .to_string()
            }
            VectorError::OutOfRange { .. } => "An f16 vector holds components up to 65504 in \
                 magnitude. Normalize the vector first, or keep it as f32 (or quantize it to \
                 i8, which scales it)."
                .to_string(),
            VectorError::Scale { .. } | VectorError::Bits { .. } => {
                "Build a quantized vector from its f32 values and let quantize() compute the \
                 codes: vector([0.1, -0.2], \"i8\")."
                    .to_string()
            }
            VectorError::NotAVector { .. } => "quantize(), similarity(), nearest() and \
                 vector_mean() take vector values. Build one \
                 from a list of numbers with vector([0.1, 0.2]) or the as_vector() step, or \
                 declare the property as a vector in the schema ({\"type\": \"array\", \
                 \"format\": \"vector\", \"element\": \"f32\"}) so a written list becomes one. \
                 A text becomes a vector through an embedder: embed(\"graph indexes\") or \
                 g.v().embed(\"name\")."
                .to_string(),
            VectorError::UnknownElement { .. } => "The element kinds are \"f32\", \"f16\", \
                 \"i8\" and \"bit\": vector([0.1, 0.2], \"i8\") or .quantize(\"f16\")."
                .to_string(),
            VectorError::DimensionMismatch { .. } => "similarity(), nearest() and \
                 vector_mean() compare vectors of one dimension. Embed the query with the model \
                 that produced the stored vectors, and declare the dimension in the schema \
                 ({\"type\": \"array\", \"format\": \"vector\", \"dimension\": 384}) so a vector \
                 of another dimension is refused when it is written."
                .to_string(),
            VectorError::BitMismatch { .. } => "A bit vector keeps only the signs, so it \
                 compares only with another bit vector. Quantize the query the same way \
                 (nearest(\"emb\", q.quantize(\"bit\"), 10)), or compare the dequantized values \
                 (values(\"emb\").quantize(\"f32\").similarity(q))."
                .to_string(),
            VectorError::HammingNeedsBits { .. } => "The hamming metric counts the differing \
                 bits of bit vectors. Quantize both sides with quantize(\"bit\"), or use the \
                 metric \"cosine\", \"dot\" or \"l2\" for f32, f16 and i8 vectors."
                .to_string(),
            VectorError::QueryNotConstant => "nearest() ranks all its inputs against one query \
                 vector. Compute it once before the search: g.v(\"d42\").values(\"emb\").as(\"q\")\
                 .v().has_label(\"doc\").nearest(\"emb\", __.select(\"q\"), 5). To score every \
                 input against its own vector, use similarity() instead."
                .to_string(),
            VectorError::MetricConflict { .. } => "nearest() ranks with one metric. Name it: \
                 nearest(\"emb\", q, 10, #{metric: \"cosine\"}), or declare the same metric for \
                 the property on every label in the schema."
                .to_string(),
            VectorError::UnknownMetric { .. } => "The metrics are \"cosine\" (the default), \
                 \"dot\", \"l2\" and \"hamming\" (bit vectors): similarity(\"emb\", q, \"l2\") or \
                 nearest(\"emb\", q, 10, #{metric: \"dot\"})."
                .to_string(),
        }
    }
}

/// The traversal error of a failed vector construction or conversion in `step`.
pub(crate) fn vector_error(step: &'static str, error: VectorError) -> Box<crate::TraverserError> {
    Box::new(crate::TraverserError::Value(
        crate::error::ValueError::Vector { step, error },
    ))
}

/// The stored data of a vector.
#[derive(Debug)]
enum Data {
    F32(Box<[f32]>),
    F16(Box<[u16]>),
    I8 { codes: Box<[i8]>, scale: f32 },
    Bit { bits: Box<[u8]>, dimension: u32 },
}

/// A vector value: an element kind, a dimension and the components behind an `Arc` (module docs).
#[derive(Clone)]
pub struct Vector(Arc<Data>);

fn check_dimension(dimension: usize) -> Result<(), VectorError> {
    if dimension == 0 || dimension > MAX_VECTOR_DIMENSION {
        return Err(VectorError::Dimension { dimension });
    }
    Ok(())
}

fn float_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        format!("{value}")
    }
}

impl Vector {
    /// An f32 vector of these components.
    pub fn from_f32(components: impl Into<Vec<f32>>) -> Result<Vector, VectorError> {
        let components: Vec<f32> = components.into();
        check_dimension(components.len())?;
        if let Some(index) = components.iter().position(|value| !value.is_finite()) {
            return Err(VectorError::NotFinite {
                index,
                value: float_text(f64::from(components[index])),
            });
        }
        Ok(Vector(Arc::new(Data::F32(components.into_boxed_slice()))))
    }

    /// An f32 vector of these f64 components (each rounded to the nearest f32; a value beyond the
    /// f32 range is refused like an infinity).
    pub fn from_f64(components: &[f64]) -> Result<Vector, VectorError> {
        check_dimension(components.len())?;
        let mut out = Vec::with_capacity(components.len());
        for (index, value) in components.iter().enumerate() {
            let narrowed = *value as f32;
            if !narrowed.is_finite() {
                return Err(VectorError::NotFinite {
                    index,
                    value: float_text(*value),
                });
            }
            out.push(narrowed);
        }
        Ok(Vector(Arc::new(Data::F32(out.into_boxed_slice()))))
    }

    /// An f16 vector of these half bit patterns (IEEE binary16).
    pub fn from_f16_bits(components: impl Into<Vec<u16>>) -> Result<Vector, VectorError> {
        let components: Vec<u16> = components.into();
        check_dimension(components.len())?;
        if let Some(index) = components
            .iter()
            .position(|bits| !half::f16_is_finite(*bits))
        {
            return Err(VectorError::NotFinite {
                index,
                value: float_text(f64::from(half::f16_to_f32(components[index]))),
            });
        }
        Ok(Vector(Arc::new(Data::F16(components.into_boxed_slice()))))
    }

    /// An i8 vector of these codes and this scale (`value = code * scale`); the scale is a
    /// finite number `>= 0` (`-0.0` is stored as `0.0`).
    pub fn from_i8(codes: impl Into<Vec<i8>>, scale: f32) -> Result<Vector, VectorError> {
        let codes: Vec<i8> = codes.into();
        check_dimension(codes.len())?;
        if !scale.is_finite() || scale < 0.0 {
            return Err(VectorError::Scale {
                scale: float_text(f64::from(scale)),
            });
        }
        // `-0.0 + 0.0` is `0.0`: one stored form for a zero scale.
        let scale = scale + 0.0;
        Ok(Vector(Arc::new(Data::I8 {
            codes: codes.into_boxed_slice(),
            scale,
        })))
    }

    /// A bit vector of `dimension` components packed into `bits` (`ceil(dimension / 8)` bytes,
    /// the first component in the most significant bit of the first byte; the padding bits of
    /// the last byte are 0).
    pub fn from_bits(bits: impl Into<Vec<u8>>, dimension: usize) -> Result<Vector, VectorError> {
        let bits: Vec<u8> = bits.into();
        check_dimension(dimension)?;
        if bits.len() != dimension.div_ceil(8) {
            return Err(VectorError::Bits {
                reason: format!(
                    "a bit vector of {dimension} components takes {} bytes, not {}",
                    dimension.div_ceil(8),
                    bits.len()
                ),
            });
        }
        let used = dimension % 8;
        if used != 0 {
            let padding = 0xffu8 >> used;
            if bits.last().is_some_and(|last| last & padding != 0) {
                return Err(VectorError::Bits {
                    reason: "the padding bits of a bit vector's last byte are 0".to_string(),
                });
            }
        }
        Ok(Vector(Arc::new(Data::Bit {
            bits: bits.into_boxed_slice(),
            dimension: dimension as u32,
        })))
    }

    /// A vector of these f32 values in the given kind (an f32 vector quantized, see
    /// [`Vector::quantize`]).
    pub fn with_element(
        components: impl Into<Vec<f32>>,
        element: VectorElement,
    ) -> Result<Vector, VectorError> {
        Vector::from_f32(components)?.quantize(element)
    }

    /// An f32 vector of a list of number values (`int64` or `float64`, each rounded to the
    /// nearest f32); the conversion of `as_vector()`, `vector([..])`, a declared vector property
    /// and the formats that carry a vector as a list.
    pub fn from_values(values: &[crate::value::ElementProperty]) -> Result<Vector, VectorError> {
        check_dimension(values.len())?;
        let mut out = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let number = match value {
                crate::value::ElementProperty::Number(number) => number
                    .as_f64()
                    .or_else(|| number.as_i64().map(|int| int as f64)),
                _ => None,
            };
            let Some(number) = number else {
                return Err(VectorError::not_a_number(
                    index,
                    crate::graph::schema::value_type_name(value),
                ));
            };
            let narrowed = number as f32;
            if !narrowed.is_finite() {
                return Err(VectorError::NotFinite {
                    index,
                    value: float_text(number),
                });
            }
            out.push(narrowed);
        }
        Ok(Vector(Arc::new(Data::F32(out.into_boxed_slice()))))
    }

    /// The element kind.
    pub fn element(&self) -> VectorElement {
        match &*self.0 {
            Data::F32(_) => VectorElement::F32,
            Data::F16(_) => VectorElement::F16,
            Data::I8 { .. } => VectorElement::I8,
            Data::Bit { .. } => VectorElement::Bit,
        }
    }

    /// The number of components.
    pub fn dimension(&self) -> usize {
        match &*self.0 {
            Data::F32(values) => values.len(),
            Data::F16(values) => values.len(),
            Data::I8 { codes, .. } => codes.len(),
            Data::Bit { dimension, .. } => *dimension as usize,
        }
    }

    /// The bytes the components take (without the `Arc` and the i8 scale).
    pub fn data_bytes(&self) -> usize {
        self.element().data_bytes(self.dimension())
    }

    /// The heap bytes this vector holds: the `Arc` allocation (counts and the data header) plus
    /// the components. Clones share them; every holder reports them (an estimate per value).
    pub fn heap_bytes(&self) -> u64 {
        (2 * std::mem::size_of::<usize>() + std::mem::size_of::<Data>() + self.data_bytes()) as u64
    }

    /// The f32 components of an f32 vector.
    pub fn as_f32(&self) -> Option<&[f32]> {
        match &*self.0 {
            Data::F32(values) => Some(values),
            Data::F16(_) | Data::I8 { .. } | Data::Bit { .. } => None,
        }
    }

    /// The half bit patterns of an f16 vector.
    pub fn as_f16_bits(&self) -> Option<&[u16]> {
        match &*self.0 {
            Data::F16(values) => Some(values),
            Data::F32(_) | Data::I8 { .. } | Data::Bit { .. } => None,
        }
    }

    /// The codes and the scale of an i8 vector.
    pub fn as_i8(&self) -> Option<(&[i8], f32)> {
        match &*self.0 {
            Data::I8 { codes, scale } => Some((codes, *scale)),
            Data::F32(_) | Data::F16(_) | Data::Bit { .. } => None,
        }
    }

    /// The packed bits of a bit vector.
    pub fn as_bits(&self) -> Option<&[u8]> {
        match &*self.0 {
            Data::Bit { bits, .. } => Some(bits),
            Data::F32(_) | Data::F16(_) | Data::I8 { .. } => None,
        }
    }

    /// The value of component `index` as an f32 (dequantized), `None` past the end.
    pub fn get(&self, index: usize) -> Option<f32> {
        if index >= self.dimension() {
            return None;
        }
        Some(match &*self.0 {
            Data::F32(values) => values[index],
            Data::F16(values) => half::f16_to_f32(values[index]),
            Data::I8 { codes, scale } => f32::from(codes[index]) * scale,
            Data::Bit { bits, .. } => bit_value(bits, index),
        })
    }

    /// The components as f32 values (dequantized), in order.
    pub fn iter_f32(&self) -> impl Iterator<Item = f32> + '_ {
        (0..self.dimension()).map(move |index| self.get(index).unwrap_or(0.0))
    }

    /// The components as f32 values (dequantized).
    pub fn to_f32_vec(&self) -> Vec<f32> {
        match &*self.0 {
            Data::F32(values) => values.to_vec(),
            Data::F16(values) => values.iter().map(|bits| half::f16_to_f32(*bits)).collect(),
            Data::I8 { codes, scale } => {
                codes.iter().map(|code| f32::from(*code) * scale).collect()
            }
            Data::Bit { bits, dimension } => (0..*dimension as usize)
                .map(|index| bit_value(bits, index))
                .collect(),
        }
    }

    /// This vector in another element kind (module docs: f32 -> f16 rounds to nearest even and
    /// refuses a component beyond 65504; f32 -> i8 is symmetric scalar quantization; f32 -> bit
    /// keeps the sign). The same kind returns the vector itself (shared, no copy); every other
    /// conversion goes through the f32 values, so `quantize(F32)` dequantizes.
    pub fn quantize(&self, element: VectorElement) -> Result<Vector, VectorError> {
        if element == self.element() {
            return Ok(self.clone());
        }
        let values = self.to_f32_vec();
        match element {
            VectorElement::F32 => Ok(Vector(Arc::new(Data::F32(values.into_boxed_slice())))),
            VectorElement::F16 => {
                let mut out = Vec::with_capacity(values.len());
                for (index, value) in values.iter().enumerate() {
                    let bits = half::f32_to_f16(*value);
                    if !half::f16_is_finite(bits) {
                        return Err(VectorError::OutOfRange {
                            index,
                            value: float_text(f64::from(*value)),
                            element,
                        });
                    }
                    out.push(bits);
                }
                Ok(Vector(Arc::new(Data::F16(out.into_boxed_slice()))))
            }
            VectorElement::I8 => {
                let max = values
                    .iter()
                    .fold(0.0f32, |max, value| max.max(value.abs()));
                let scale = max / I8_MAX_CODE;
                let codes: Vec<i8> = if scale == 0.0 {
                    vec![0; values.len()]
                } else {
                    values
                        .iter()
                        .map(|value| (value / scale).round().clamp(-I8_MAX_CODE, I8_MAX_CODE) as i8)
                        .collect()
                };
                Vector::from_i8(codes, scale)
            }
            VectorElement::Bit => {
                let mut bits = vec![0u8; values.len().div_ceil(8)];
                for (index, value) in values.iter().enumerate() {
                    if *value > 0.0 {
                        bits[index / 8] |= 0x80 >> (index % 8);
                    }
                }
                Ok(Vector(Arc::new(Data::Bit {
                    bits: bits.into_boxed_slice(),
                    dimension: values.len() as u32,
                })))
            }
        }
    }

    /// The bits used for equality and hashing of a float component: `-0.0` as `0.0`.
    fn canonical_f32_bits(value: f32) -> u32 {
        (value + 0.0).to_bits()
    }

    /// The literal text with at most `limit` components shown (`None`: all of them).
    pub(crate) fn literal(&self, limit: Option<usize>) -> String {
        use fmt::Write as _;
        let dimension = self.dimension();
        let shown = limit.map_or(dimension, |limit| limit.min(dimension));
        let mut out = String::with_capacity(16 + shown * 8);
        out.push_str("vector([");
        for (index, value) in self.iter_f32().take(shown).enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(&float_literal(value));
        }
        if shown < dimension {
            out.push_str(", …");
        }
        out.push(']');
        if self.element() != VectorElement::F32 {
            let _ = write!(out, ", \"{}\"", self.element().name());
        }
        out.push(')');
        if shown < dimension {
            let _ = write!(out, " ({dimension} items)");
        }
        out
    }

    /// The short description `vector[768 f32]` the result views show instead of the components.
    pub fn summary(&self) -> String {
        format!("vector[{} {}]", self.dimension(), self.element().name())
    }

    /// The summary and the first `components` values: `vector[768 f32] [0.1, -0.2, …]` (a table
    /// cell, a preview).
    pub fn preview(&self, components: usize) -> String {
        let shown: Vec<String> = self
            .iter_f32()
            .take(components)
            .map(|value| tagged::short_f64(value).to_string())
            .collect();
        let more = if self.dimension() > components {
            ", …"
        } else {
            ""
        };
        format!("{} [{}{more}]", self.summary(), shown.join(", "))
    }
}

/// A float as a Rhai literal: always with a decimal point or an exponent.
fn float_literal(value: f32) -> String {
    let abs = value.abs();
    let text = if abs != 0.0 && !(1e-6..1e16).contains(&abs) {
        format!("{value:e}")
    } else {
        format!("{value}")
    };
    if text.contains(['.', 'e']) {
        text
    } else {
        format!("{text}.0")
    }
}

fn bit_value(bits: &[u8], index: usize) -> f32 {
    if bits[index / 8] & (0x80 >> (index % 8)) != 0 {
        1.0
    } else {
        -1.0
    }
}

impl PartialEq for Vector {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        match (&*self.0, &*other.0) {
            // By value: `-0.0 == 0.0`; NaN cannot occur.
            (Data::F32(a), Data::F32(b)) => a == b,
            (Data::F16(a), Data::F16(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(a, b)| half::f16_to_f32(*a) == half::f16_to_f32(*b))
            }
            (
                Data::I8 {
                    codes: a,
                    scale: sa,
                },
                Data::I8 {
                    codes: b,
                    scale: sb,
                },
            ) => sa == sb && a == b,
            (
                Data::Bit {
                    bits: a,
                    dimension: da,
                },
                Data::Bit {
                    bits: b,
                    dimension: db,
                },
            ) => da == db && a == b,
            (Data::F32(_) | Data::F16(_) | Data::I8 { .. } | Data::Bit { .. }, _) => false,
        }
    }
}

impl Eq for Vector {}

impl Vector {
    /// Exact identity: the same element kind, dimension and component BITS (`-0.0` differs
    /// from `0.0`, unlike `==`). The dictionary of a `dedup` rule shares a value only with an
    /// identical one (`value/identity.rs`); identical vectors hash alike under [`Hash`].
    pub(crate) fn identical(&self, other: &Vector) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        match (&*self.0, &*other.0) {
            (Data::F32(a), Data::F32(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(a, b)| a.to_bits() == b.to_bits())
            }
            (Data::F16(a), Data::F16(b)) => a == b,
            (
                Data::I8 {
                    codes: a,
                    scale: sa,
                },
                Data::I8 {
                    codes: b,
                    scale: sb,
                },
            ) => sa.to_bits() == sb.to_bits() && a == b,
            (
                Data::Bit {
                    bits: a,
                    dimension: da,
                },
                Data::Bit {
                    bits: b,
                    dimension: db,
                },
            ) => da == db && a == b,
            (Data::F32(_) | Data::F16(_) | Data::I8 { .. } | Data::Bit { .. }, _) => false,
        }
    }
}

impl Hash for Vector {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.element().hash(state);
        self.dimension().hash(state);
        match &*self.0 {
            Data::F32(values) => {
                for value in values.iter() {
                    state.write_u32(Vector::canonical_f32_bits(*value));
                }
            }
            Data::F16(values) => {
                for bits in values.iter() {
                    state.write_u32(Vector::canonical_f32_bits(half::f16_to_f32(*bits)));
                }
            }
            Data::I8 { codes, scale } => {
                state.write_u32(Vector::canonical_f32_bits(*scale));
                for code in codes.iter() {
                    state.write_i8(*code);
                }
            }
            Data::Bit { bits, .. } => state.write(bits),
        }
    }
}

impl Ord for Vector {
    fn cmp(&self, other: &Self) -> Ordering {
        self.element()
            .cmp(&other.element())
            .then_with(|| self.dimension().cmp(&other.dimension()))
            .then_with(|| {
                // Finite values only, so `partial_cmp` is total here (`-0.0` equals `0.0`).
                for (a, b) in self.iter_f32().zip(other.iter_f32()) {
                    match a.partial_cmp(&b).unwrap_or(Ordering::Equal) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                }
                Ordering::Equal
            })
            .then_with(|| match (&*self.0, &*other.0) {
                // Equal values with different scales (only possible for an all-zero vector):
                // the scale, then the codes decide, so `Equal` agrees with `==`.
                (
                    Data::I8 {
                        codes: a,
                        scale: sa,
                    },
                    Data::I8 {
                        codes: b,
                        scale: sb,
                    },
                ) => sa
                    .partial_cmp(sb)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.cmp(b)),
                (Data::F32(_) | Data::F16(_) | Data::I8 { .. } | Data::Bit { .. }, _) => {
                    Ordering::Equal
                }
            })
    }
}

impl PartialOrd for Vector {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The DSL literal: `vector([0.1, 0.2])` for f32, `vector([0.1, 0.2], "i8")` for the other kinds
/// (the dequantized values; evaluating it quantizes them back to the same vector). Every
/// component is written: use [`Vector::summary`] for a short text.
impl fmt::Display for Vector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.literal(None))
    }
}

impl fmt::Debug for Vector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.literal(Some(crate::value::literal::MAX_LITERAL_ITEMS)))
    }
}

impl TryFrom<Vec<f32>> for Vector {
    type Error = VectorError;

    fn try_from(components: Vec<f32>) -> Result<Self, Self::Error> {
        Vector::from_f32(components)
    }
}

impl TryFrom<&[f32]> for Vector {
    type Error = VectorError;

    fn try_from(components: &[f32]) -> Result<Self, Self::Error> {
        Vector::from_f32(components.to_vec())
    }
}

#[cfg(test)]
mod tests;
