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
//! **Embeddings.** The DSL step `embed()`, the function `embed(text)` and text queries
//! (`nearest("emb", "text", 5)`) compute vectors with the built-in `hash-ngram/1` (hashed words
//! and character n-grams, no model file, the same bits on every target) or with an embedding
//! model kept in the graph's database (book page "Embedding Models in a Store": a model enters
//! a database only by an import, `persist::Store::import_model` (feature `persist`),
//! CLI `graphersal embed-model import`). `g.embedders()` lists them. A computed vector is
//! stored as a plain value: replay, recovery and loads never call an embedder. Their failures
//! are [`EmbedError`].

pub use crate::value::EmbedError;
pub use crate::value::vector::MAX_VECTOR_DIMENSION;
pub use crate::value::{Vector, VectorElement, VectorError, VectorMetric};
