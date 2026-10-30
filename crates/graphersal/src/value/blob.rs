//! [`BlobRef`]: a reference to an immutable, content-addressed file of a Store (format spec 3,
//! tag 21, and 4.6 "Blobs").

use std::borrow::Cow;
use std::fmt;

use crate::value::physical::PhysicalValue;

/// The longest hash a [`BlobRef`] may carry, in bytes (format spec 3, tag 21: `hash_len`
/// 1..=64).
pub const MAX_BLOB_HASH_BYTES: usize = 64;

/// The hash algorithm that names a blob (format spec 3, tag 21 `hash_algo`).
///
/// `#[non_exhaustive]`: a later version may compute another algorithm. A reference written by
/// such a newer version is kept as [`BlobHashAlgo::Unknown`] with its code: decoded, carried,
/// never verified nor read.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum BlobHashAlgo {
    /// SHA-256 (code 1, 32-byte hash): the algorithm this version writes.
    Sha256,
    /// A code this version does not know (2..=255): a newer writer's algorithm.
    Unknown(u8),
}

impl BlobHashAlgo {
    /// The algorithm of a code (format spec 3): `None` for `0` (invalid); `1` is
    /// [`BlobHashAlgo::Sha256`]; every other code is [`BlobHashAlgo::Unknown`].
    pub const fn from_code(code: u8) -> Option<BlobHashAlgo> {
        match code {
            0 => None,
            1 => Some(BlobHashAlgo::Sha256),
            other => Some(BlobHashAlgo::Unknown(other)),
        }
    }

    /// The code written into a file.
    pub const fn code(self) -> u8 {
        match self {
            BlobHashAlgo::Sha256 => 1,
            BlobHashAlgo::Unknown(code) => code,
        }
    }

    /// The algorithm's name: `sha256`, or `algo-<code>` for an unknown one (the name of its
    /// directory under `blobs/`, format spec 4.6).
    pub fn name(self) -> String {
        match self {
            BlobHashAlgo::Sha256 => "sha256".to_owned(),
            BlobHashAlgo::Unknown(code) => format!("algo-{code}"),
        }
    }

    /// The hash length this algorithm has, when this version knows it (32 for SHA-256).
    pub const fn hash_len(self) -> Option<usize> {
        match self {
            BlobHashAlgo::Sha256 => Some(32),
            BlobHashAlgo::Unknown(_) => None,
        }
    }
}

/// A reference to a blob: an immutable file of a Store named by the hash of its content
/// (format spec 4.6), with the content's length and CRC-32C.
///
/// The hash is the blob's identity (computed when the content is written or arrives from
/// outside, and by `verify`); the CRC-32C checks every read; the length is checked and used
/// for memory decisions before reading. A `BlobRef` is a value type of the persist format
/// (tag 21): in this version it appears only in catalog definition payloads (model
/// definitions, host kinds). A graph refuses it as an element property value.
///
/// Two references are equal when every field is.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct BlobRef {
    algo: BlobHashAlgo,
    hash: Box<[u8]>,
    length: u64,
    crc32c: u32,
}

/// Why [`BlobRef::new`] refused its parts.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum BlobRefError {
    /// The hash is empty or longer than [`MAX_BLOB_HASH_BYTES`].
    HashLength {
        /// The length given.
        len: usize,
    },
    /// The hash length does not match the algorithm (SHA-256 has 32 bytes).
    AlgoLength {
        /// The algorithm.
        algo: BlobHashAlgo,
        /// The length given.
        len: usize,
    },
    /// A hash text that is not lowercase or uppercase hex of whole bytes.
    NotHex,
}

impl fmt::Display for BlobRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlobRefError::HashLength { len } => {
                write!(
                    f,
                    "a blob hash has 1 to {MAX_BLOB_HASH_BYTES} bytes, not {len}"
                )
            }
            BlobRefError::AlgoLength { algo, len } => write!(
                f,
                "a {} blob hash has {} bytes, not {len}",
                algo.name(),
                algo.hash_len().unwrap_or(0)
            ),
            BlobRefError::NotHex => write!(f, "a blob hash text must be hex digits of whole bytes"),
        }
    }
}

impl std::error::Error for BlobRefError {}

impl BlobRef {
    /// A reference from its parts.
    ///
    /// # Errors
    ///
    /// [`BlobRefError`] when the hash is empty, longer than [`MAX_BLOB_HASH_BYTES`] or its
    /// length does not match the algorithm.
    pub fn new(
        algo: BlobHashAlgo,
        hash: &[u8],
        length: u64,
        crc32c: u32,
    ) -> Result<BlobRef, BlobRefError> {
        if hash.is_empty() || hash.len() > MAX_BLOB_HASH_BYTES {
            return Err(BlobRefError::HashLength { len: hash.len() });
        }
        if let Some(expected) = algo.hash_len()
            && expected != hash.len()
        {
            return Err(BlobRefError::AlgoLength {
                algo,
                len: hash.len(),
            });
        }
        Ok(BlobRef {
            algo,
            hash: hash.into(),
            length,
            crc32c,
        })
    }

    /// The hash algorithm.
    pub fn hash_algo(&self) -> BlobHashAlgo {
        self.algo
    }

    /// The hash bytes.
    pub fn hash(&self) -> &[u8] {
        &self.hash
    }

    /// The hash as lowercase hex (the blob's file name).
    pub fn hash_hex(&self) -> String {
        hex(&self.hash)
    }

    /// The content length in bytes.
    pub fn length(&self) -> u64 {
        self.length
    }

    /// The CRC-32C of the content.
    pub fn crc32c(&self) -> u32 {
        self.crc32c
    }

    /// The blob's name relative to the store's `blobs/` directory (format spec 4.6):
    /// `<2 hex>/<hash hex>` for SHA-256, `<algo name>/<2 hex>/<hash hex>` for another algorithm.
    pub fn relative_path(&self) -> String {
        let hex = self.hash_hex();
        let prefix = &hex[..2.min(hex.len())];
        match self.algo {
            BlobHashAlgo::Sha256 => format!("{prefix}/{hex}"),
            BlobHashAlgo::Unknown(_) => format!("{}/{prefix}/{hex}", self.algo.name()),
        }
    }

    /// Parses a hex hash text into bytes.
    pub(crate) fn parse_hex(text: &str) -> Result<Vec<u8>, BlobRefError> {
        if !text.len().is_multiple_of(2) {
            return Err(BlobRefError::NotHex);
        }
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        };
        text.as_bytes()
            .chunks(2)
            .map(|pair| match (digit(pair[0]), digit(pair[1])) {
                (Some(high), Some(low)) => Ok(high << 4 | low),
                _ => Err(BlobRefError::NotHex),
            })
            .collect()
    }

    /// The heap bytes this reference owns (its hash).
    pub(crate) fn heap_bytes(&self) -> u64 {
        self.hash.len() as u64
    }
}

/// The tag of a blob reference in the tagged JSON form (the serialized `ChangeSet`).
pub(crate) const BLOB_TAG: &str = "graphersal:BlobRef";

/// The JSON payload of a blob reference: `{"algo": "sha256", "hash": hex, "length": n,
/// "crc32c": n}`; an unknown algorithm is named `algo-<code>`.
pub(crate) fn payload(blob: &BlobRef) -> PhysicalValue<'static> {
    PhysicalValue::Object(vec![
        (
            Cow::Borrowed("algo"),
            PhysicalValue::String(Cow::Owned(blob.algo.name())),
        ),
        (
            Cow::Borrowed("hash"),
            PhysicalValue::String(Cow::Owned(blob.hash_hex())),
        ),
        // A length beyond i64 cannot be written by this version; saturate (never reached).
        (
            Cow::Borrowed("length"),
            PhysicalValue::Int(i64::try_from(blob.length).unwrap_or(i64::MAX)),
        ),
        (
            Cow::Borrowed("crc32c"),
            PhysicalValue::Int(i64::from(blob.crc32c)),
        ),
    ])
}

/// The physical (untyped JSON) form of a blob reference: the tagged form, output only.
pub(crate) fn lower(blob: &BlobRef) -> PhysicalValue<'static> {
    PhysicalValue::Object(vec![
        (
            Cow::Borrowed("@type"),
            PhysicalValue::String(Cow::Borrowed(BLOB_TAG)),
        ),
        (Cow::Borrowed("@value"), payload(blob)),
    ])
}

/// A blob reference read from its JSON payload (see [`payload`]); exact: the four keys, nothing
/// else.
#[cfg_attr(not(feature = "serde"), allow(dead_code))]
pub(crate) fn from_payload(value: &PhysicalValue<'_>) -> Result<BlobRef, String> {
    let PhysicalValue::Object(fields) = value else {
        return Err(format!("{BLOB_TAG} needs an object payload"));
    };
    let (mut algo, mut hash, mut length, mut crc) = (None, None, None, None);
    for (key, field) in fields {
        match (key.as_ref(), field) {
            ("algo", PhysicalValue::String(text)) if algo.is_none() => {
                algo = Some(if text == "sha256" {
                    BlobHashAlgo::Sha256
                } else {
                    text.strip_prefix("algo-")
                        .and_then(|code| code.parse::<u8>().ok())
                        .and_then(BlobHashAlgo::from_code)
                        .ok_or_else(|| format!("{BLOB_TAG}: unknown algorithm \"{text}\""))?
                });
            }
            ("hash", PhysicalValue::String(text)) if hash.is_none() => {
                hash = Some(BlobRef::parse_hex(text).map_err(|err| format!("{BLOB_TAG}: {err}"))?);
            }
            ("length", PhysicalValue::Int(n)) if length.is_none() && *n >= 0 => {
                length = Some(*n as u64);
            }
            ("crc32c", PhysicalValue::Int(n)) if crc.is_none() => {
                crc = Some(u32::try_from(*n).map_err(|_| format!("{BLOB_TAG}: crc32c {n}"))?);
            }
            (other, _) => {
                return Err(format!(
                    "{BLOB_TAG}: unexpected or malformed key \"{other}\""
                ));
            }
        }
    }
    match (algo, hash, length, crc) {
        (Some(algo), Some(hash), Some(length), Some(crc)) => {
            BlobRef::new(algo, &hash, length, crc).map_err(|err| format!("{BLOB_TAG}: {err}"))
        }
        _ => Err(format!(
            "{BLOB_TAG} needs the keys algo, hash, length and crc32c"
        )),
    }
}

/// Collects every blob reference `value` holds, at any depth, into `out` (one entry per
/// occurrence).
pub(crate) fn collect_blobs(value: &crate::value::ElementProperty, out: &mut Vec<BlobRef>) {
    use crate::value::ElementProperty;
    match value {
        ElementProperty::Blob(blob) => out.push((**blob).clone()),
        ElementProperty::Array(items) => items.iter().for_each(|item| collect_blobs(item, out)),
        ElementProperty::Object(map) => map.values().for_each(|item| collect_blobs(item, out)),
        ElementProperty::Map(map) => map.values().for_each(|item| collect_blobs(item, out)),
        ElementProperty::Null
        | ElementProperty::String(_)
        | ElementProperty::Number(_)
        | ElementProperty::Boolean(_)
        | ElementProperty::Uuid(_)
        | ElementProperty::Temporal(_)
        | ElementProperty::Vector(_)
        | ElementProperty::Vertex(_)
        | ElementProperty::Edge(_)
        | ElementProperty::Compressed(_)
        | ElementProperty::Deduped(_) => {}
    }
}

/// Lowercase hex of bytes.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

impl fmt::Display for BlobRef {
    /// `blob(sha256:<hex>, <length> bytes)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "blob({}:{}, {} bytes)",
            self.algo.name(),
            self.hash_hex(),
            self.length
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sha256_reference_needs_32_bytes() {
        assert!(BlobRef::new(BlobHashAlgo::Sha256, &[1; 32], 3, 4).is_ok());
        assert_eq!(
            BlobRef::new(BlobHashAlgo::Sha256, &[1; 31], 3, 4),
            Err(BlobRefError::AlgoLength {
                algo: BlobHashAlgo::Sha256,
                len: 31
            })
        );
        assert!(BlobRef::new(BlobHashAlgo::Unknown(9), &[1; 5], 3, 4).is_ok());
        assert!(BlobRef::new(BlobHashAlgo::Unknown(9), &[], 3, 4).is_err());
        assert!(BlobRef::new(BlobHashAlgo::Unknown(9), &[0; 65], 3, 4).is_err());
        assert_eq!(BlobHashAlgo::from_code(0), None);
    }

    #[test]
    fn names_and_hex_round_trip() {
        let blob = BlobRef::new(BlobHashAlgo::Sha256, &[0xab; 32], 3, 4).unwrap();
        assert_eq!(blob.relative_path(), format!("ab/{}", "ab".repeat(32)));
        assert_eq!(
            BlobRef::parse_hex(&blob.hash_hex()).unwrap(),
            vec![0xab; 32]
        );
        let other = BlobRef::new(BlobHashAlgo::Unknown(7), &[0x01, 0x02], 3, 4).unwrap();
        assert_eq!(other.relative_path(), "algo-7/01/0102");
        assert!(BlobRef::parse_hex("0g").is_err());
        assert_eq!(from_payload(&payload(&blob)).unwrap(), blob);
        assert_eq!(from_payload(&payload(&other)).unwrap(), other);
    }
}
