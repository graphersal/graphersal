//! Vectors (embeddings): the vector value type a property or a traversal value can hold.
//!
//! For query writers and integrators that build or read vectors in Rust: a [`Vector`] is stored
//! as [`ElementProperty::Vector`](crate::ElementProperty::Vector) and travels in a traversal as
//! that property value ([`TraverserValue::Computed`](crate::TraverserValue::Computed)). The guide
//! is the book page "Vectors". The engine stores, converts and compares vectors; the
//! embedders below compute them from text.
//!
//! | Element | Component | Bytes per component |
//! |---|---|---|
//! | [`VectorElement::F32`] | IEEE single precision | 4 |
//! | [`VectorElement::F16`] | IEEE half precision | 2 |
//! | [`VectorElement::I8`] | i8 code times one f32 scale per vector | 1 |
//! | [`VectorElement::Bit`] | one sign bit (`1.0` / `-1.0`) | 1/8 |
//!
//! A vector has 1 to [`MAX_VECTOR_DIMENSION`] components, never NaN or an infinity. It is
//! immutable and shares its data (a clone is a reference count). [`Vector::quantize`] converts
//! between the element kinds; a schema can declare a property stored quantized from the start
//! ([`VectorSchema`](crate::schema::VectorSchema)).
//!
//! ```rust
//! use graphersal::vector::{Vector, VectorElement};
//!
//! let embedding = Vector::from_f32(vec![0.5, -1.0, 0.25]).unwrap();
//! let compact = embedding.quantize(VectorElement::I8).unwrap();
//! assert_eq!(compact.element(), VectorElement::I8);
//! assert_eq!(compact.dimension(), 3);
//! assert_eq!(compact.data_bytes(), 3);
//! assert_eq!(embedding.to_string(), "vector([0.5, -1.0, 0.25])");
//! ```

//!
//! **Embeddings.** An [`Embedder`] turns texts into vectors; a host registers embedders in an
//! [`Embedders`] registry and hands it to its traversals
//! ([`GraphTraversalSource::with_embedders`](crate::GraphTraversalSource::with_embedders), the
//! script entry points `script::set_engine_embedders` and `script::set_scope_embedders`,
//! feature `script`). The built-in [`HashNgram`]
//! (`hash-ngram/1`: hashed words and character n-grams, no model file, the same bits on every
//! target) is always registered; the DSL step `embed()` and the function `embed(text)` call the
//! registry's embedders. The engine never calls a model on its own, and a computed vector is
//! stored as a plain value: replay, recovery and loads never call an embedder.

pub use crate::value::vector::MAX_VECTOR_DIMENSION;
pub use crate::value::vector::embed::{DEFAULT_MAX_BATCH, MAX_EMBEDDER_NAME_BYTES};
pub use crate::value::{
    EmbedContext, EmbedError, Embedder, EmbedderInfo, Embedders, EmbeddersBuilder, HashNgram,
};
/// Feature `model2vec` (native targets only): a Model2Vec static embedding model loaded from a
/// local directory.
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
pub use crate::value::{Model2Vec, Model2VecOptions};
pub use crate::value::{Vector, VectorElement, VectorError, VectorMetric};
