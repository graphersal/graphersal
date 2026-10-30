//! Blobs: immutable, content-addressed files of a Store, referenced by a [`BlobRef`].
//!
//! For hosts that keep large binary content (model files, later user BLOB values) beside a
//! graph. A blob is written once into a Store ([`Store::write_blob`](crate::persist::Store),
//! feature `persist`), named by the SHA-256 of its content (`blobs/<2 hex>/<hash>`, format spec
//! 4.6), and referenced from a catalog definition's payload by its [`BlobRef`] (hash, length,
//! CRC-32C). The Store keeps a blob exactly as long as some retained history references it
//! (prune, attic removal and rollback `delete` remove the others) and every copy of the store
//! carries the blobs its history references. A damaged or missing blob is never store damage:
//! only what needs its bytes fails. The book page is "Blobs" in the persistence part.
//!
//! In this version a [`BlobRef`] is a value of catalog payloads only: a graph refuses it as an
//! element property value (`GraphError::InvalidPropertyValue`, kind `"blob reference"`).
//!
//! ```rust
//! use graphersal::blob::{BlobHashAlgo, BlobRef};
//!
//! let blob = BlobRef::new(BlobHashAlgo::Sha256, &[0xab; 32], 1024, 7).unwrap();
//! assert_eq!(blob.length(), 1024);
//! assert_eq!(blob.hash_hex().len(), 64);
//! ```

/// Feature `persist`: a written blob not yet referenced by a durable commit, and a store's
/// blob usage.
#[cfg(feature = "persist")]
pub use crate::persist::store::blobs::{BlobCopy, BlobUsage, PendingBlob, blob_hash_count};
pub use crate::value::blob::{BlobHashAlgo, BlobRef, BlobRefError, MAX_BLOB_HASH_BYTES};
