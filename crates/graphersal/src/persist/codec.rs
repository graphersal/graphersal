//! The byte-level codec shared by every persistence section (format spec sections 2 and 3):
//! fixed little-endian integers, LEB128 varints, strings, the tagged value encoding, CRC-32C and
//! chunk compression.
//!
//! Decoding goes through [`Reader`], a bounds-checked cursor: every length is compared with the
//! bytes that are actually left before anything is allocated, so hostile input can never make
//! the decoder allocate more than the input's own size (times the compression ratio bound) or
//! read out of bounds.

use compact_str::CompactString;

use crate::value::number::{N, Number};
use crate::value::temporal::zone::ZoneId;
use crate::value::temporal::{Date, DateTime, Duration, LocalDateTime, OffsetTime, Temporal, Time};
use crate::value::vector::MAX_VECTOR_DIMENSION;
use crate::value::{ElementProperty, PropertyMap, Vector, VectorElement};

/// The file format version every file of this build writes (format spec 15).
pub(crate) const FORMAT_VERSION: u32 = 3;

/// The oldest file format version this build reads: version 1 (Graphersal 0.1.x) differs from
/// version 2 only by the temporal value tags it does not have, version 2 (0.2.x) from version 3
/// only by the nanosecond temporal value tags (format spec 3, 15).
pub(crate) const MIN_FORMAT_VERSION: u32 = 1;

/// The first file format version with the temporal value tags (9 to 13) and the vector tag (14).
pub(crate) const TEMPORAL_FORMAT_VERSION: u32 = 2;

/// The first file format version with the nanosecond temporal value tags (15 to 20): an
/// `offsettime` and a `datetime` with a zone name among them.
pub(crate) const NANOS_FORMAT_VERSION: u32 = 3;

/// Nanoseconds per second, for the `svar seconds` + `uvar nanos` pairs of tags 15 to 20.
const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// Whether this build reads files of format `version` (1 to 3).
#[inline]
pub(crate) fn readable_version(version: u32) -> bool {
    (MIN_FORMAT_VERSION..=FORMAT_VERSION).contains(&version)
}

/// The file format version a decoded file was written with; the current one by default (what
/// this build writes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileVersion(pub(crate) u32);

impl Default for FileVersion {
    fn default() -> Self {
        FileVersion(FORMAT_VERSION)
    }
}

/// The largest uncompressed chunk or record body this build writes or accepts (1 GiB): far
/// above the 1 MiB chunk target, so only a single giant element comes near it.
pub(crate) const MAX_BLOCK_BYTES: usize = 1 << 30;

/// The deepest value nesting the encoder writes; a guard against stack exhaustion on values
/// built through the Rust API (the engine's own writes stay within `traversal.max_value_depth`).
const MAX_ENCODE_DEPTH: usize = 4096;

/// A decode failure inside one section: the byte offset within the section and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodeError {
    pub(crate) offset: u64,
    pub(crate) reason: String,
    /// The file format version found, when the failure is a version NEWER than
    /// [`FORMAT_VERSION`] in an otherwise intact header (format spec 15): written by a newer
    /// Graphersal, never damage.
    pub(crate) newer_version: Option<u32>,
    /// Content behind a valid checksum that this version cannot interpret (format spec 16):
    /// written by a newer Graphersal, never damage ([`super::PersistError::UnsupportedContent`]).
    pub(crate) unsupported: bool,
}

impl DecodeError {
    pub(crate) fn new(offset: u64, reason: impl Into<String>) -> Self {
        Self {
            offset,
            reason: reason.into(),
            newer_version: None,
            unsupported: false,
        }
    }

    /// Content whose checksum is valid but that this version cannot interpret (format spec
    /// 16): a schema keyword, a payload field or a record body of a newer Graphersal.
    pub(crate) fn unsupported(offset: u64, reason: impl Into<String>) -> Self {
        Self {
            unsupported: true,
            ..Self::new(offset, reason)
        }
    }

    /// A header whose checksum is valid but whose file format version `found` is newer than
    /// [`FORMAT_VERSION`] (format spec 15); `what` names the kind of file.
    pub(crate) fn newer(offset: u64, what: &str, found: u32) -> Self {
        Self {
            offset,
            reason: format!(
                "{what} format version {found}: written by a newer Graphersal (this build \
                 reads up to version {FORMAT_VERSION})"
            ),
            newer_version: Some(found),
            unsupported: false,
        }
    }

    /// The version check of an intact header (magic and checksum valid) of `what`: a version
    /// newer than [`FORMAT_VERSION`] is [`Self::newer`], one older than
    /// [`MIN_FORMAT_VERSION`] (never written by a release) is damage.
    pub(crate) fn check_version(offset: u64, what: &str, version: u32) -> DecodeResult<()> {
        if version > FORMAT_VERSION {
            return Err(Self::newer(offset, what, version));
        }
        if !readable_version(version) {
            return Err(Self::new(
                offset,
                format!("{what} format version {version}"),
            ));
        }
        Ok(())
    }

    /// The error as a [`super::PersistError::Corrupt`] of `location`, the offset shifted by
    /// `base` (the section's position in its file); a newer file format version is
    /// [`super::PersistError::UnsupportedVersion`] instead, never damage.
    pub(crate) fn at(self, location: impl Into<String>, base: u64) -> super::PersistError {
        if let Some(found) = self.newer_version {
            return super::PersistError::UnsupportedVersion {
                location: location.into(),
                found,
                supported: FORMAT_VERSION,
            };
        }
        if self.unsupported {
            return super::PersistError::unsupported_content(
                location,
                base.saturating_add(self.offset),
                self.reason,
            );
        }
        super::PersistError::corrupt(location, base.saturating_add(self.offset), self.reason)
    }
}

pub(crate) type DecodeResult<T> = Result<T, DecodeError>;

/// CRC-32C (Castagnoli) of `bytes`.
#[inline]
pub(crate) fn crc(bytes: &[u8]) -> u32 {
    crc32c::crc32c(bytes)
}

/// CRC-32C continued over `bytes` from the running value `crc`.
#[inline]
pub(crate) fn crc_append(crc: u32, bytes: &[u8]) -> u32 {
    crc32c::crc32c_append(crc, bytes)
}

/// The CRC-32C of `a | b` from the CRC of `a`, the CRC of `b` and the length of `b`: a file
/// whose head is known only after its body (a segment's header) is checksummed without
/// holding the body.
#[inline]
pub(crate) fn crc_combine(crc_a: u32, crc_b: u32, len_b: u64) -> u32 {
    // The snapshot encoder refuses a segment longer than `usize::MAX` (32-bit targets).
    crc32c::crc32c_combine(crc_a, crc_b, usize::try_from(len_b).unwrap_or(usize::MAX))
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

#[inline]
pub(crate) fn put_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

#[inline]
pub(crate) fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[inline]
pub(crate) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[inline]
pub(crate) fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[inline]
pub(crate) fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Unsigned LEB128.
#[inline]
pub(crate) fn put_uvar(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Zigzag + LEB128.
#[inline]
pub(crate) fn put_svar(out: &mut Vec<u8>, value: i64) {
    put_uvar(out, ((value << 1) ^ (value >> 63)) as u64);
}

/// `uvar len` + UTF-8 bytes.
#[inline]
pub(crate) fn put_str(out: &mut Vec<u8>, value: &str) {
    put_uvar(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

/// An optional string: `uvar 0` = none, else `uvar len+1` + bytes (the principal of a commit,
/// an edge label in a journal record).
pub(crate) fn put_opt_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => put_uvar(out, 0),
        Some(value) => {
            put_uvar(out, value.len() as u64 + 1);
            out.extend_from_slice(value.as_bytes());
        }
    }
}

/// How object keys (and, in segments, labels and property keys) are written: inline strings
/// (journal records) or references into a chunk's string table (segments).
pub(crate) trait KeyWriter {
    fn put_key(&mut self, out: &mut Vec<u8>, key: &str);
}

/// Keys as inline strings.
pub(crate) struct InlineKeys;

impl KeyWriter for InlineKeys {
    #[inline]
    fn put_key(&mut self, out: &mut Vec<u8>, key: &str) {
        put_str(out, key);
    }
}

/// Value tags (format spec 3).
pub(crate) mod tag {
    pub(crate) const NULL: u8 = 0;
    pub(crate) const FALSE: u8 = 1;
    pub(crate) const TRUE: u8 = 2;
    pub(crate) const INT64: u8 = 3;
    pub(crate) const FLOAT64: u8 = 4;
    pub(crate) const STRING: u8 = 5;
    pub(crate) const UUID: u8 = 6;
    pub(crate) const ARRAY: u8 = 7;
    pub(crate) const OBJECT: u8 = 8;
    /// `date`: `svar` days since 1970-01-01 (file format version 2; also what version 3 writes).
    pub(crate) const DATE: u8 = 9;
    /// `localdatetime`: `svar` microseconds since 1970-01-01T00:00:00 (wall clock); read only
    /// (version 3 writes [`LOCAL_DATE_TIME_NS`]).
    pub(crate) const LOCAL_DATE_TIME: u8 = 10;
    /// `datetime`: `svar` microseconds since the epoch (UTC), `svar` offset seconds; read only.
    pub(crate) const DATE_TIME: u8 = 11;
    /// `time`: `uvar` microseconds since midnight; read only.
    pub(crate) const TIME: u8 = 12;
    /// `duration`: `svar` months, `svar` days, `svar` microseconds; read only.
    pub(crate) const DURATION: u8 = 13;
    /// `vector`: `u8` element kind (0 f32, 1 f16, 2 i8, 3 bit), `uvar` dimension
    /// (1..=65536), for i8 the `f32` scale, then the packed components little endian (f32: 4
    /// bytes, f16: 2, i8: 1, bit: `ceil(dimension / 8)` bytes in all, first component in the
    /// most significant bit) (file format version 2).
    pub(crate) const VECTOR: u8 = 14;
    /// `localdatetime` (file format version 3): `svar` seconds since 1970-01-01T00:00:00
    /// rounded down (wall clock), `uvar` nanoseconds of that second (0..10^9).
    pub(crate) const LOCAL_DATE_TIME_NS: u8 = 15;
    /// `datetime` without a zone name (version 3): `svar` seconds since the epoch (UTC) rounded
    /// down, `uvar` nanoseconds of that second, `svar` offset seconds.
    pub(crate) const DATE_TIME_NS: u8 = 16;
    /// `datetime` with an IANA zone name (version 3): the fields of [`DATE_TIME_NS`], then the
    /// name as a key (a string-table reference in a chunk, an inline string in a record).
    pub(crate) const ZONED_DATE_TIME: u8 = 17;
    /// `time` (version 3): `uvar` nanoseconds since midnight.
    pub(crate) const TIME_NS: u8 = 18;
    /// `offsettime` (version 3): `uvar` nanoseconds since midnight (local), `svar` offset
    /// seconds.
    pub(crate) const OFFSET_TIME: u8 = 19;
    /// `duration` (version 3): `svar` months, `svar` days, `svar` seconds of the time part
    /// rounded down, `uvar` nanoseconds of that second.
    pub(crate) const DURATION_NS: u8 = 20;
    /// "Absent" in the `before` of a journal SetProperty (never a value).
    pub(crate) const ABSENT: u8 = 255;
}

/// Writes one storable value. Elements and maps with non-string keys are not storable and fail.
pub(crate) fn put_value<K: KeyWriter>(
    out: &mut Vec<u8>,
    value: &ElementProperty,
    keys: &mut K,
) -> Result<(), String> {
    put_value_at(out, value, keys, 0)
}

fn put_value_at<K: KeyWriter>(
    out: &mut Vec<u8>,
    value: &ElementProperty,
    keys: &mut K,
    depth: usize,
) -> Result<(), String> {
    match value {
        ElementProperty::Null => put_u8(out, tag::NULL),
        ElementProperty::Boolean(false) => put_u8(out, tag::FALSE),
        ElementProperty::Boolean(true) => put_u8(out, tag::TRUE),
        ElementProperty::Number(number) => match &number.n {
            N::NegInt(int) => {
                put_u8(out, tag::INT64);
                put_svar(out, *int);
            }
            N::Float(float) => {
                put_u8(out, tag::FLOAT64);
                out.extend_from_slice(&float.into_inner().to_bits().to_le_bytes());
            }
        },
        ElementProperty::String(text) => {
            put_u8(out, tag::STRING);
            put_str(out, text);
        }
        // Disk holds the plain value: the chunks are compressed as a whole (D5, D11).
        ElementProperty::Compressed(text) => {
            put_u8(out, tag::STRING);
            text.with_text(|text| put_str(out, text));
        }
        ElementProperty::Deduped(text) => {
            put_u8(out, tag::STRING);
            put_str(out, text.as_str());
        }
        ElementProperty::Uuid(uuid) => {
            put_u8(out, tag::UUID);
            out.extend_from_slice(&uuid.to_be_bytes());
        }
        ElementProperty::Array(items) => {
            if depth >= MAX_ENCODE_DEPTH {
                return Err(format!(
                    "a value nests deeper than {MAX_ENCODE_DEPTH} levels"
                ));
            }
            put_u8(out, tag::ARRAY);
            put_uvar(out, items.len() as u64);
            for item in items {
                put_value_at(out, item, keys, depth + 1)?;
            }
        }
        ElementProperty::Object(object) => {
            if depth >= MAX_ENCODE_DEPTH {
                return Err(format!(
                    "a value nests deeper than {MAX_ENCODE_DEPTH} levels"
                ));
            }
            put_u8(out, tag::OBJECT);
            put_uvar(out, object.len() as u64);
            for (key, item) in object.iter() {
                keys.put_key(out, key);
                put_value_at(out, item, keys, depth + 1)?;
            }
        }
        ElementProperty::Temporal(value) => put_temporal(out, value, keys),
        ElementProperty::Vector(vector) => put_vector(out, vector),
        ElementProperty::Vertex(_) => return Err("a vertex is not a storable value".into()),
        ElementProperty::Edge(_) => return Err("an edge is not a storable value".into()),
        ElementProperty::Map(_) => {
            return Err("a map with a non-string key is not a storable value".into());
        }
    }
    Ok(())
}

/// Writes a vector with its tag (format spec 3, file format version 2).
fn put_vector(out: &mut Vec<u8>, vector: &Vector) {
    put_u8(out, tag::VECTOR);
    put_u8(out, vector.element().code());
    put_uvar(out, vector.dimension() as u64);
    out.reserve(vector.data_bytes() + 4);
    if let Some(values) = vector.as_f32() {
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
    } else if let Some(values) = vector.as_f16_bits() {
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
    } else if let Some((codes, scale)) = vector.as_i8() {
        out.extend_from_slice(&scale.to_le_bytes());
        out.extend(codes.iter().map(|code| code.to_le_bytes()[0]));
    } else if let Some(bits) = vector.as_bits() {
        out.extend_from_slice(bits);
    }
}

/// Reads the payload of a vector tag; an unknown element kind, a dimension out of range or a
/// component the constructors refuse (NaN, an infinity, a negative scale, padding bits) is
/// invalid (damage), and so is the tag in a file of format version 1. The payload length is
/// checked against the remaining bytes before anything is allocated.
fn read_vector(reader: &mut Reader<'_>, start: u64) -> DecodeResult<Vector> {
    if reader.version < TEMPORAL_FORMAT_VERSION {
        return Err(DecodeError::new(
            start,
            format!(
                "value tag {} (a vector) in a file of format version {}",
                tag::VECTOR,
                reader.version
            ),
        ));
    }
    let invalid = |why: String| DecodeError::new(start, format!("invalid vector value: {why}"));
    let code = reader.u8()?;
    let element = VectorElement::from_code(code)
        .ok_or_else(|| invalid(format!("unknown element kind {code}")))?;
    let dimension = reader.uvar()?;
    if dimension == 0 || dimension > MAX_VECTOR_DIMENSION as u64 {
        return Err(invalid(format!("dimension {dimension}")));
    }
    let dimension = dimension as usize;
    let vector = match element {
        VectorElement::F32 => {
            let bytes = reader.bytes(dimension * 4)?;
            Vector::from_f32(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|chunk| f32::from_le_bytes(*chunk))
                    .collect::<Vec<_>>(),
            )
        }
        VectorElement::F16 => {
            let bytes = reader.bytes(dimension * 2)?;
            Vector::from_f16_bits(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_le_bytes(*chunk))
                    .collect::<Vec<_>>(),
            )
        }
        VectorElement::I8 => {
            let scale = f32::from_bits(reader.u32()?);
            let bytes = reader.bytes(dimension)?;
            Vector::from_i8(
                bytes
                    .iter()
                    .map(|byte| i8::from_le_bytes([*byte]))
                    .collect::<Vec<_>>(),
                scale,
            )
        }
        VectorElement::Bit => {
            let bytes = reader.bytes(dimension.div_ceil(8))?;
            Vector::from_bits(bytes.to_vec(), dimension)
        }
    };
    vector.map_err(|err| invalid(err.to_string()))
}

/// Writes a temporal value with its tag (format spec 3, file format version 3): a `date` as tag
/// 9, every other kind in its nanosecond tag (15 to 20), the one form version 3 writes for it.
/// Every engine value has an encoding, so nothing is refused. A zone name is written as a key
/// through `keys` (the chunk's string table, or inline in a journal record), never as the
/// build-local [`ZoneId`].
fn put_temporal<K: KeyWriter>(out: &mut Vec<u8>, value: &Temporal, keys: &mut K) {
    match *value {
        Temporal::Date(date) => {
            put_u8(out, tag::DATE);
            put_svar(out, date.days_since_epoch() as i64);
        }
        Temporal::LocalDateTime(local) => {
            put_u8(out, tag::LOCAL_DATE_TIME_NS);
            put_seconds_nanos(out, local.nanos());
        }
        Temporal::DateTime(value) => match value.zone_name() {
            None => {
                put_u8(out, tag::DATE_TIME_NS);
                put_seconds_nanos(out, value.epoch_nanos());
                put_svar(out, value.offset_seconds() as i64);
            }
            Some(name) => {
                put_u8(out, tag::ZONED_DATE_TIME);
                put_seconds_nanos(out, value.epoch_nanos());
                put_svar(out, value.offset_seconds() as i64);
                keys.put_key(out, name);
            }
        },
        Temporal::Time(time) => {
            put_u8(out, tag::TIME_NS);
            // Within `0..86_400 * 10^9` by construction.
            put_uvar(out, time.nanos_of_day() as u64);
        }
        Temporal::OffsetTime(time) => {
            put_u8(out, tag::OFFSET_TIME);
            put_uvar(out, time.time().nanos_of_day() as u64);
            put_svar(out, time.offset_seconds() as i64);
        }
        Temporal::Duration(duration) => {
            put_u8(out, tag::DURATION_NS);
            put_svar(out, duration.months() as i64);
            put_svar(out, duration.days() as i64);
            put_seconds_nanos(out, duration.nanos());
        }
    }
}

/// Writes nanoseconds as `svar` seconds rounded down and `uvar` nanoseconds of that second
/// (0..10^9). The callers' values lie within an `i64` of seconds by construction: the years
/// -9999..9999 (about ±3.2 * 10^11 seconds) and a duration's time part
/// ([`Duration::MIN_NANOS`]`..=`[`Duration::MAX_NANOS`]).
fn put_seconds_nanos(out: &mut Vec<u8>, nanos: i128) {
    put_svar(out, nanos.div_euclid(NANOS_PER_SECOND) as i64);
    put_uvar(out, nanos.rem_euclid(NANOS_PER_SECOND) as u64);
}

/// Writes a property map: `uvar count | (key | value)*`, insertion order kept.
pub(crate) fn put_properties<K: KeyWriter>(
    out: &mut Vec<u8>,
    properties: Option<&PropertyMap>,
    keys: &mut K,
) -> Result<(), String> {
    let Some(properties) = properties else {
        put_uvar(out, 0);
        return Ok(());
    };
    put_uvar(out, properties.len() as u64);
    for (key, value) in properties.iter() {
        keys.put_key(out, key);
        put_value(out, value, keys).map_err(|reason| format!("property '{key}': {reason}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// A bounds-checked cursor over one section's bytes.
#[derive(Debug, Clone)]
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// The file format version of the bytes: a temporal value tag needs version 2 or later.
    version: u32,
}

impl<'a> Reader<'a> {
    /// A reader of bytes of the current file format version.
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            version: FORMAT_VERSION,
        }
    }

    /// The same reader for bytes of file format `version` (read from the file's header).
    pub(crate) fn with_version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }

    /// The file format version of the bytes.
    #[inline]
    pub(crate) fn version(&self) -> u32 {
        self.version
    }

    /// The current offset within the section.
    #[inline]
    pub(crate) fn offset(&self) -> u64 {
        self.pos as u64
    }

    /// Bytes left.
    #[inline]
    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    #[inline]
    pub(crate) fn error(&self, reason: impl Into<String>) -> DecodeError {
        DecodeError::new(self.offset(), reason)
    }

    #[inline]
    pub(crate) fn bytes(&mut self, len: usize) -> DecodeResult<&'a [u8]> {
        if len > self.remaining() {
            return Err(self.error(format!(
                "{len} bytes needed, {} left (truncated)",
                self.remaining()
            )));
        }
        let slice = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    #[inline]
    fn array<const L: usize>(&mut self) -> DecodeResult<[u8; L]> {
        let bytes = self.bytes(L)?;
        let mut out = [0u8; L];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    #[inline]
    pub(crate) fn u8(&mut self) -> DecodeResult<u8> {
        Ok(self.array::<1>()?[0])
    }

    #[inline]
    pub(crate) fn u16(&mut self) -> DecodeResult<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    #[inline]
    pub(crate) fn u32(&mut self) -> DecodeResult<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    #[inline]
    pub(crate) fn u64(&mut self) -> DecodeResult<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    #[inline]
    pub(crate) fn i64(&mut self) -> DecodeResult<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    #[inline]
    pub(crate) fn u128_be(&mut self) -> DecodeResult<u128> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    /// Unsigned LEB128; more than 10 bytes or a value beyond `u64` is an error.
    pub(crate) fn uvar(&mut self) -> DecodeResult<u64> {
        let start = self.offset();
        let mut value: u64 = 0;
        for index in 0..10u32 {
            let byte = self.u8()?;
            let bits = u64::from(byte & 0x7F);
            if index == 9 && bits > 1 {
                return Err(DecodeError::new(start, "varint overflows u64"));
            }
            value |= bits << (7 * index);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(DecodeError::new(start, "varint longer than 10 bytes"))
    }

    /// Zigzag + LEB128.
    pub(crate) fn svar(&mut self) -> DecodeResult<i64> {
        let raw = self.uvar()?;
        Ok(((raw >> 1) as i64) ^ -((raw & 1) as i64))
    }

    /// A `uvar` that counts items of at least `min_item_bytes` each: larger than what the
    /// remaining bytes can hold is an error, so the count is safe to preallocate with.
    pub(crate) fn count(&mut self, min_item_bytes: usize) -> DecodeResult<usize> {
        let start = self.offset();
        let count = self.uvar()?;
        let max = self.remaining() / min_item_bytes.max(1);
        if count > max as u64 {
            return Err(DecodeError::new(
                start,
                format!("count {count} exceeds what the remaining bytes can hold"),
            ));
        }
        Ok(count as usize)
    }

    /// `uvar len` + UTF-8 bytes, borrowed.
    pub(crate) fn str(&mut self) -> DecodeResult<&'a str> {
        let start = self.offset();
        let len = self.uvar()?;
        if len > self.remaining() as u64 {
            return Err(DecodeError::new(
                start,
                format!("string of {len} bytes runs past the end"),
            ));
        }
        let bytes = self.bytes(len as usize)?;
        std::str::from_utf8(bytes).map_err(|_| DecodeError::new(start, "invalid UTF-8 in a string"))
    }

    /// See [`put_opt_str`].
    pub(crate) fn opt_str(&mut self) -> DecodeResult<Option<&'a str>> {
        let start = self.offset();
        let len = self.uvar()?;
        if len == 0 {
            return Ok(None);
        }
        let len = len - 1;
        if len > self.remaining() as u64 {
            return Err(DecodeError::new(
                start,
                format!("string of {len} bytes runs past the end"),
            ));
        }
        let bytes = self.bytes(len as usize)?;
        std::str::from_utf8(bytes)
            .map(Some)
            .map_err(|_| DecodeError::new(start, "invalid UTF-8 in a string"))
    }
}

/// How keys are read back: inline strings or references into a chunk's string table.
pub(crate) trait KeyReader {
    fn key(&self, reader: &mut Reader<'_>) -> DecodeResult<CompactString>;
}

impl KeyReader for InlineKeys {
    #[inline]
    fn key(&self, reader: &mut Reader<'_>) -> DecodeResult<CompactString> {
        reader.str().map(CompactString::from)
    }
}

/// Keys as references into a chunk's string table.
pub(crate) struct TableKeys<'t>(pub(crate) &'t [CompactString]);

impl KeyReader for TableKeys<'_> {
    #[inline]
    fn key(&self, reader: &mut Reader<'_>) -> DecodeResult<CompactString> {
        let start = reader.offset();
        let index = reader.uvar()?;
        usize::try_from(index)
            .ok()
            .and_then(|index| self.0.get(index))
            .cloned()
            .ok_or_else(|| {
                DecodeError::new(
                    start,
                    format!("string table reference {index} out of range"),
                )
            })
    }
}

/// Reads one value; containers deeper than `max_depth` are an error.
pub(crate) fn read_value<K: KeyReader>(
    reader: &mut Reader<'_>,
    keys: &K,
    max_depth: usize,
) -> DecodeResult<ElementProperty> {
    let tag = reader.u8()?;
    read_tagged_value(reader, tag, keys, max_depth, 0)
}

/// Reads the value after its `tag` byte.
pub(crate) fn read_tagged_value<K: KeyReader>(
    reader: &mut Reader<'_>,
    tag: u8,
    keys: &K,
    max_depth: usize,
    depth: usize,
) -> DecodeResult<ElementProperty> {
    let start = reader.offset().saturating_sub(1);
    Ok(match tag {
        tag::NULL => ElementProperty::Null,
        tag::FALSE => ElementProperty::Boolean(false),
        tag::TRUE => ElementProperty::Boolean(true),
        tag::INT64 => ElementProperty::Number(Number::from_i64(reader.svar()?)),
        tag::FLOAT64 => {
            let bits = reader.u64()?;
            // Built directly: keeps every bit pattern (NaN payloads) exactly as stored.
            ElementProperty::Number(Number {
                n: N::Float(ordered_float::OrderedFloat(f64::from_bits(bits))),
            })
        }
        tag::STRING => ElementProperty::String(CompactString::from(reader.str()?)),
        tag::UUID => ElementProperty::Uuid(reader.u128_be()?),
        tag::DATE | tag::LOCAL_DATE_TIME | tag::DATE_TIME | tag::TIME | tag::DURATION => {
            ElementProperty::Temporal(read_temporal(reader, tag, start)?)
        }
        tag::LOCAL_DATE_TIME_NS
        | tag::DATE_TIME_NS
        | tag::ZONED_DATE_TIME
        | tag::TIME_NS
        | tag::OFFSET_TIME
        | tag::DURATION_NS => {
            ElementProperty::Temporal(read_temporal_ns(reader, tag, keys, start)?)
        }
        tag::VECTOR => ElementProperty::Vector(read_vector(reader, start)?),
        tag::ARRAY => {
            if depth >= max_depth {
                return Err(DecodeError::new(
                    start,
                    format!("value nests deeper than {max_depth} levels"),
                ));
            }
            let count = reader.count(1)?;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                let tag = reader.u8()?;
                items.push(read_tagged_value(reader, tag, keys, max_depth, depth + 1)?);
            }
            ElementProperty::Array(items)
        }
        tag::OBJECT => {
            if depth >= max_depth {
                return Err(DecodeError::new(
                    start,
                    format!("value nests deeper than {max_depth} levels"),
                ));
            }
            let count = reader.count(2)?;
            let mut object = PropertyMap::with_capacity(count);
            for _ in 0..count {
                let key_at = reader.offset();
                let key = keys.key(reader)?;
                let tag = reader.u8()?;
                let value = read_tagged_value(reader, tag, keys, max_depth, depth + 1)?;
                if object.insert(key, value).is_some() {
                    return Err(DecodeError::new(key_at, "duplicate key in an object"));
                }
            }
            ElementProperty::Object(Box::new(object))
        }
        other => {
            return Err(DecodeError::new(
                start,
                format!("unknown value tag {other}"),
            ));
        }
    })
}

/// Reads the payload of a temporal value tag; a value outside the supported range is invalid
/// (damage), and so is the tag in a file of format version 1.
fn read_temporal(reader: &mut Reader<'_>, tag: u8, start: u64) -> DecodeResult<Temporal> {
    if reader.version < TEMPORAL_FORMAT_VERSION {
        return Err(DecodeError::new(
            start,
            format!(
                "value tag {tag} (a temporal value) in a file of format version {}",
                reader.version
            ),
        ));
    }
    let invalid = |what: &str| DecodeError::new(start, format!("invalid {what} value"));
    let int32 = |value: i64, what: &str| i32::try_from(value).map_err(|_| invalid(what));
    Ok(match tag {
        tag::DATE => {
            let days = int32(reader.svar()?, "date")?;
            Temporal::Date(Date::from_days_since_epoch(days).map_err(|_| invalid("date"))?)
        }
        tag::LOCAL_DATE_TIME => Temporal::LocalDateTime(
            LocalDateTime::from_micros(reader.svar()?).map_err(|_| invalid("localdatetime"))?,
        ),
        tag::DATE_TIME => {
            let micros = reader.svar()?;
            let offset = int32(reader.svar()?, "datetime")?;
            Temporal::DateTime(
                DateTime::from_micros(micros, offset).map_err(|_| invalid("datetime"))?,
            )
        }
        tag::TIME => {
            let micros = i64::try_from(reader.uvar()?).map_err(|_| invalid("time"))?;
            Temporal::Time(Time::from_micros_of_day(micros).map_err(|_| invalid("time"))?)
        }
        _ => {
            let months = int32(reader.svar()?, "duration")?;
            let days = int32(reader.svar()?, "duration")?;
            Temporal::Duration(Duration::new(months, days, reader.svar()?))
        }
    })
}

/// Reads the payload of a nanosecond temporal value tag (15 to 20, file format version 3); a
/// value outside the supported range or a nanosecond field of 10^9 or more is invalid (damage),
/// and so is the tag in a file of an older format version. A zone name this build's time zone
/// database does not know (a newer database's name) is content of a newer Graphersal behind a
/// valid checksum: [`DecodeError::unsupported`], never damage (format spec 16 rule 9).
fn read_temporal_ns<K: KeyReader>(
    reader: &mut Reader<'_>,
    tag: u8,
    keys: &K,
    start: u64,
) -> DecodeResult<Temporal> {
    if reader.version < NANOS_FORMAT_VERSION {
        return Err(DecodeError::new(
            start,
            format!(
                "value tag {tag} (a temporal value) in a file of format version {}",
                reader.version
            ),
        ));
    }
    let invalid = |what: &str| DecodeError::new(start, format!("invalid {what} value"));
    let int32 = |value: i64, what: &str| i32::try_from(value).map_err(|_| invalid(what));
    let seconds_nanos = |reader: &mut Reader<'_>, what: &str| -> DecodeResult<i128> {
        let seconds = reader.svar()?;
        let nanos = reader.uvar()?;
        if nanos >= NANOS_PER_SECOND as u64 {
            return Err(invalid(what));
        }
        Ok(seconds as i128 * NANOS_PER_SECOND + nanos as i128)
    };
    let nanos_of_day = |reader: &mut Reader<'_>, what: &str| -> DecodeResult<Time> {
        let nanos = i64::try_from(reader.uvar()?).map_err(|_| invalid(what))?;
        Time::from_nanos_of_day(nanos).map_err(|_| invalid(what))
    };
    Ok(match tag {
        tag::LOCAL_DATE_TIME_NS => {
            let nanos = seconds_nanos(reader, "localdatetime")?;
            Temporal::LocalDateTime(
                LocalDateTime::from_nanos(nanos).map_err(|_| invalid("localdatetime"))?,
            )
        }
        tag::DATE_TIME_NS | tag::ZONED_DATE_TIME => {
            let nanos = seconds_nanos(reader, "datetime")?;
            let offset = int32(reader.svar()?, "datetime")?;
            let value = DateTime::from_nanos(nanos, offset).map_err(|_| invalid("datetime"))?;
            if tag == tag::DATE_TIME_NS {
                Temporal::DateTime(value)
            } else {
                let name_at = reader.offset();
                let name = keys.key(reader)?;
                let Some(zone) = ZoneId::lookup(&name) else {
                    return Err(DecodeError::unsupported(
                        name_at,
                        format!(
                            "the time zone '{name}' of a datetime is not in this build's time \
                             zone database (written by a newer Graphersal)"
                        ),
                    ));
                };
                // The offset stays as written; the name is not used to recompute it.
                Temporal::DateTime(value.with_zone_id(Some(zone)))
            }
        }
        tag::TIME_NS => Temporal::Time(nanos_of_day(reader, "time")?),
        tag::OFFSET_TIME => {
            let time = nanos_of_day(reader, "offsettime")?;
            let offset = int32(reader.svar()?, "offsettime")?;
            Temporal::OffsetTime(OffsetTime::new(time, offset).map_err(|_| invalid("offsettime"))?)
        }
        _ => {
            let months = int32(reader.svar()?, "duration")?;
            let days = int32(reader.svar()?, "duration")?;
            let nanos = seconds_nanos(reader, "duration")?;
            Temporal::Duration(
                Duration::with_nanos(months, days, nanos).map_err(|_| invalid("duration"))?,
            )
        }
    })
}

/// Reads a property map written by [`put_properties`]; a repeated key is an error.
pub(crate) fn read_properties<K: KeyReader>(
    reader: &mut Reader<'_>,
    keys: &K,
    max_depth: usize,
) -> DecodeResult<PropertyMap> {
    let count = reader.count(2)?;
    let mut properties = PropertyMap::with_capacity(count);
    for _ in 0..count {
        let key_at = reader.offset();
        let key = keys.key(reader)?;
        let value = read_value(reader, keys, max_depth)?;
        if properties.insert(key, value).is_some() {
            return Err(DecodeError::new(key_at, "duplicate property key"));
        }
    }
    Ok(properties)
}

/// The shapes of the element property maps of one chunk, by their key-reference sequence: the
/// elements of a chunk repeat a few key sequences, so a decoded map is built `Shaped` directly (an
/// exact value vector plus a shared key list) without an `IndexMap` and without hashing a key.
/// The graph adopts the shape into its own table when the element is stored.
#[derive(Default)]
pub(crate) struct ChunkShapes {
    /// The key references of the map being read.
    ids: Vec<usize>,
    /// The reader offset of each of those references (for a duplicate-key error).
    offsets: Vec<u64>,
    /// The sequence and shape of the previous map.
    last: Option<(
        Box<[usize]>,
        std::sync::Arc<crate::value::property_map::Shape>,
    )>,
    /// Every sequence of the chunk seen so far.
    known: std::collections::HashMap<
        Box<[usize]>,
        std::sync::Arc<crate::value::property_map::Shape>,
        std::hash::BuildHasherDefault<fnv::FnvHasher>,
    >,
}

impl ChunkShapes {
    /// The shape of the key references just read; a new sequence is checked for a repeated key
    /// (by text, so two references to equal strings count as a repeat).
    fn shape_of(
        &mut self,
        table: &[CompactString],
    ) -> DecodeResult<std::sync::Arc<crate::value::property_map::Shape>> {
        use crate::value::property_map::Shape;
        if let Some((ids, shape)) = &self.last
            && **ids == *self.ids
        {
            return Ok(std::sync::Arc::clone(shape));
        }
        let shape = match self.known.get(self.ids.as_slice()) {
            Some(shape) => std::sync::Arc::clone(shape),
            None => {
                let mut keys: Vec<CompactString> = Vec::with_capacity(self.ids.len());
                let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
                for (at, (index, offset)) in self.ids.iter().zip(&self.offsets).enumerate() {
                    let key = table.get(*index).ok_or_else(|| {
                        DecodeError::new(*offset, "string table reference out of range")
                    })?;
                    let repeated = if self.ids.len() <= 32 {
                        keys.iter().take(at).any(|other| other == key)
                    } else {
                        !seen.insert(key.as_str())
                    };
                    if repeated {
                        return Err(DecodeError::new(*offset, "duplicate property key"));
                    }
                    keys.push(key.clone());
                }
                let shape = std::sync::Arc::new(Shape::new(keys.into_boxed_slice(), 0));
                self.known
                    .insert(self.ids.as_slice().into(), std::sync::Arc::clone(&shape));
                shape
            }
        };
        self.last = Some((self.ids.as_slice().into(), std::sync::Arc::clone(&shape)));
        Ok(shape)
    }
}

/// Reads an element's property map written by [`put_properties`] with string-table keys, built
/// `Shaped` through `shapes` (see [`ChunkShapes`]); a repeated key is an error.
pub(crate) fn read_element_properties(
    reader: &mut Reader<'_>,
    table: &[CompactString],
    max_depth: usize,
    shapes: &mut ChunkShapes,
) -> DecodeResult<PropertyMap> {
    let keys = TableKeys(table);
    let count = reader.count(2)?;
    shapes.ids.clear();
    shapes.offsets.clear();
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        let key_at = reader.offset();
        let index = reader.uvar()?;
        let index = usize::try_from(index)
            .ok()
            .filter(|index| *index < table.len())
            .ok_or_else(|| {
                DecodeError::new(
                    key_at,
                    format!("string table reference {index} out of range"),
                )
            })?;
        shapes.ids.push(index);
        shapes.offsets.push(key_at);
        values.push(read_value(reader, &keys, max_depth)?);
    }
    let shape = shapes.shape_of(table)?;
    Ok(PropertyMap::from_shaped(shape, values))
}

// ---------------------------------------------------------------------------------------------
// Compression
// ---------------------------------------------------------------------------------------------

/// A chunk or record body codec (format spec 2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Codec {
    /// Stored as is.
    None,
    /// LZ4 block format (`lz4_flex`), the default.
    #[default]
    Lz4,
    /// zstd (`ruzstd`, feature `persist-zstd`).
    #[cfg(feature = "persist-zstd")]
    Zstd,
}

impl Codec {
    pub(crate) fn byte(self) -> u8 {
        match self {
            Codec::None => 0,
            Codec::Lz4 => 1,
            #[cfg(feature = "persist-zstd")]
            Codec::Zstd => 2,
        }
    }
}

/// Below this many bytes a block is stored uncompressed.
pub(crate) const COMPRESS_FROM: usize = 4096;

/// Compresses `raw` with `codec` when it is at least [`COMPRESS_FROM`] bytes and compression
/// helps; returns the codec byte actually used and the stored bytes (`None`: store `raw`).
pub(crate) fn compress(raw: &[u8], codec: Codec) -> (u8, Option<Vec<u8>>) {
    if raw.len() < COMPRESS_FROM {
        return (0, None);
    }
    let stored = match codec {
        Codec::None => return (0, None),
        Codec::Lz4 => lz4_flex::block::compress(raw),
        #[cfg(feature = "persist-zstd")]
        Codec::Zstd => {
            ruzstd::encoding::compress_to_vec(raw, ruzstd::encoding::CompressionLevel::Fastest)
        }
    };
    if stored.len() >= raw.len() {
        return (0, None);
    }
    (codec.byte(), Some(stored))
}

/// The LZ4 block format expands at most about 255 times (a match length is extended one byte
/// per 255 bytes); a declared size beyond this bound cannot be genuine.
const LZ4_MAX_RATIO: usize = 255;

/// Whether `codec` is a block codec of the format that this build cannot decode (zstd, codec
/// 2, without the `persist-zstd` feature): such a block is unsupported, never damage (format
/// spec 2).
pub(crate) fn codec_unavailable(codec: u8) -> bool {
    codec == 2 && !cfg!(feature = "persist-zstd")
}

/// The failure of [`decompress`] at `offset`: unsupported for a codec this build lacks
/// ([`codec_unavailable`]), else invalid bytes.
pub(crate) fn decompress_error(codec: u8, offset: u64, reason: String) -> DecodeError {
    if codec_unavailable(codec) {
        DecodeError::unsupported(offset, reason)
    } else {
        DecodeError::new(offset, reason)
    }
}

/// Decompresses `stored` (codec byte `codec`) into exactly `uncompressed` bytes. The size is
/// checked against [`MAX_BLOCK_BYTES`] and the codec's expansion bound before anything is
/// allocated.
pub(crate) fn decompress(
    codec: u8,
    stored: &[u8],
    uncompressed: usize,
) -> Result<std::borrow::Cow<'_, [u8]>, String> {
    if uncompressed > MAX_BLOCK_BYTES {
        return Err(format!(
            "declared size {uncompressed} exceeds the limit of {MAX_BLOCK_BYTES} bytes"
        ));
    }
    match codec {
        0 => {
            if stored.len() != uncompressed {
                return Err(format!(
                    "uncompressed block of {} bytes declares {uncompressed}",
                    stored.len()
                ));
            }
            Ok(std::borrow::Cow::Borrowed(stored))
        }
        1 => {
            if uncompressed
                > stored
                    .len()
                    .saturating_mul(LZ4_MAX_RATIO)
                    .saturating_add(64)
            {
                return Err(format!(
                    "declared size {uncompressed} is beyond what {} LZ4 bytes can hold",
                    stored.len()
                ));
            }
            let mut out = vec![0u8; uncompressed];
            let written = lz4_flex::block::decompress_into(stored, &mut out)
                .map_err(|err| format!("LZ4: {err}"))?;
            if written != uncompressed {
                return Err(format!(
                    "LZ4 block decodes to {written} bytes, {uncompressed} declared"
                ));
            }
            Ok(std::borrow::Cow::Owned(out))
        }
        #[cfg(feature = "persist-zstd")]
        2 => {
            use std::io::Read;
            let mut source = stored;
            let decoder = ruzstd::decoding::StreamingDecoder::new(&mut source)
                .map_err(|err| format!("zstd: {err}"))?;
            let mut out = Vec::new();
            decoder
                .take(uncompressed as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|err| format!("zstd: {err}"))?;
            if out.len() != uncompressed {
                return Err(format!(
                    "zstd block decodes to {} bytes, {uncompressed} declared",
                    out.len()
                ));
            }
            Ok(std::borrow::Cow::Owned(out))
        }
        #[cfg(not(feature = "persist-zstd"))]
        2 => Err("zstd block (codec 2), but this build has no `persist-zstd` feature".into()),
        other => Err(format!("unknown codec {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip_and_reject_overlong_input() {
        for value in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut out = Vec::new();
            put_uvar(&mut out, value);
            assert_eq!(Reader::new(&out).uvar().unwrap(), value);
        }
        for value in [0i64, -1, 1, i64::MIN, i64::MAX] {
            let mut out = Vec::new();
            put_svar(&mut out, value);
            assert_eq!(Reader::new(&out).svar().unwrap(), value);
        }
        assert!(Reader::new(&[0xFF; 11]).uvar().is_err());
        // Ten bytes whose last one carries more than the u64's top bit.
        let mut overflow = vec![0xFF; 9];
        overflow.push(0x02);
        assert!(Reader::new(&overflow).uvar().is_err());
        assert!(Reader::new(&[0x80]).uvar().is_err());
    }

    #[test]
    fn counts_beyond_the_input_are_rejected() {
        let mut out = Vec::new();
        put_uvar(&mut out, 1 << 40);
        assert!(Reader::new(&out).count(1).is_err());
    }

    #[test]
    fn decompress_bounds_the_declared_size() {
        assert!(decompress(1, &[0u8; 4], 1 << 20).is_err());
        assert!(decompress(0, &[1, 2, 3], 4).is_err());
        assert!(decompress(9, &[], 0).is_err());
        assert!(decompress(1, &[0u8; 4], MAX_BLOCK_BYTES + 1).is_err());
    }

    /// A zone name in a chunk is a string-table reference (format spec 3, tag 17): written and
    /// read through the table; a name this build does not know is unsupported content, a
    /// reference out of range damage; the nanosecond tags in a version 2 chunk are damage.
    #[test]
    fn zone_names_go_through_the_string_table() {
        struct Table(Vec<CompactString>);
        impl KeyWriter for Table {
            fn put_key(&mut self, out: &mut Vec<u8>, key: &str) {
                let index = match self.0.iter().position(|k| k == key) {
                    Some(index) => index,
                    None => {
                        self.0.push(key.into());
                        self.0.len() - 1
                    }
                };
                put_uvar(out, index as u64);
            }
        }
        let zoned: DateTime = "2024-01-01T10:00:00.000000001[America/Argentina/ComodRivadavia]"
            .parse()
            .unwrap();
        let value = ElementProperty::Temporal(Temporal::DateTime(zoned));
        let mut table = Table(Vec::new());
        let mut out = Vec::new();
        put_value(&mut out, &value, &mut table).unwrap();
        put_value(&mut out, &value, &mut table).unwrap();
        assert_eq!(table.0.len(), 1, "one table entry for both values");
        assert_eq!(out[0], tag::ZONED_DATE_TIME);
        let read = |bytes: &[u8], table: &[CompactString], version: u32| {
            let mut reader = Reader::new(bytes).with_version(version);
            read_value(&mut reader, &TableKeys(table), 8)
        };
        let back = read(&out, &table.0, FORMAT_VERSION).unwrap();
        match back {
            ElementProperty::Temporal(Temporal::DateTime(back)) => {
                assert_eq!(back.zone_name(), zoned.zone_name());
                assert_eq!(back.epoch_nanos(), zoned.epoch_nanos());
                assert_eq!(back.offset_seconds(), zoned.offset_seconds());
            }
            other => panic!("{other:?}"),
        }
        let newer = vec![CompactString::from("Mars/Olympus_Mons")];
        let err = read(&out, &newer, FORMAT_VERSION).unwrap_err();
        assert!(err.unsupported, "{err:?}");
        assert!(err.reason.contains("Mars/Olympus_Mons"), "{}", err.reason);
        let err = read(&out, &[], FORMAT_VERSION).unwrap_err();
        assert!(
            !err.unsupported && err.reason.contains("out of range"),
            "{err:?}"
        );
        let err = read(&out, &table.0, 2).unwrap_err();
        assert!(
            !err.unsupported && err.reason.contains("format version 2"),
            "{err:?}"
        );
    }

    /// A block of a codec of the format that this build lacks is unsupported, never damage
    /// (format spec 2); an unknown codec byte or invalid bytes are damage.
    #[test]
    fn a_codec_this_build_lacks_is_unsupported_not_damage() {
        assert_eq!(codec_unavailable(2), !cfg!(feature = "persist-zstd"));
        assert!(!codec_unavailable(1) && !codec_unavailable(9));
        assert!(!decompress_error(9, 0, "x".into()).unsupported);
        assert!(!decompress_error(1, 0, "x".into()).unsupported);
        #[cfg(not(feature = "persist-zstd"))]
        {
            let reason = decompress(2, &[1, 2, 3], 3).unwrap_err();
            assert!(decompress_error(2, 0, reason).unsupported);
        }
    }
}

/// Element property maps decoded with string-table keys are built `Shaped` per chunk.
#[cfg(test)]
mod chunk_shape_tests {
    use super::*;

    /// `(table index, value)` entries of one map: the encoded bytes.
    fn encode(entries: &[(u64, i64)]) -> Vec<u8> {
        let mut out = Vec::new();
        put_uvar(&mut out, entries.len() as u64);
        for (index, value) in entries {
            put_uvar(&mut out, *index);
            put_value(&mut out, &ElementProperty::from(*value), &mut InlineKeys).unwrap();
        }
        out
    }

    fn table(keys: &[&str]) -> Vec<CompactString> {
        keys.iter().map(|k| CompactString::from(*k)).collect()
    }

    fn read(
        bytes: &[u8],
        table: &[CompactString],
        shapes: &mut ChunkShapes,
    ) -> DecodeResult<PropertyMap> {
        read_element_properties(&mut Reader::new(bytes), table, 8, shapes)
    }

    #[test]
    fn maps_of_one_sequence_share_a_shape_and_read_back() {
        let table = table(&["name", "age", "city"]);
        let mut shapes = ChunkShapes::default();
        let a = read(&encode(&[(0, 1), (1, 2)]), &table, &mut shapes).unwrap();
        let b = read(&encode(&[(0, 3), (1, 4)]), &table, &mut shapes).unwrap();
        let c = read(&encode(&[(1, 5), (0, 6)]), &table, &mut shapes).unwrap();
        let d = read(&encode(&[(0, 7), (1, 8)]), &table, &mut shapes).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            a.shape().unwrap(),
            b.shape().unwrap()
        ));
        assert!(!std::sync::Arc::ptr_eq(
            a.shape().unwrap(),
            c.shape().unwrap()
        ));
        // Back to a known sequence: the shape of the chunk's table, not a new one.
        assert!(std::sync::Arc::ptr_eq(
            a.shape().unwrap(),
            d.shape().unwrap()
        ));
        assert_eq!(
            a,
            PropertyMap::from([("name", ElementProperty::from(1)), ("age", 2.into())])
        );
        assert_eq!(
            c.keys().map(|k| k.as_str()).collect::<Vec<_>>(),
            ["age", "name"]
        );
        let empty = read(&encode(&[]), &table, &mut shapes).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn a_repeated_key_is_an_error_at_its_offset() {
        let table = table(&["name", "age", "name"]);
        let mut shapes = ChunkShapes::default();
        let bytes = encode(&[(0, 1), (0, 2)]);
        let err = read(&bytes, &table, &mut shapes).unwrap_err();
        assert_eq!(err.reason, "duplicate property key");
        // The second reference: after the count, the first key and its value.
        let mut first = Vec::new();
        put_uvar(&mut first, 2);
        put_uvar(&mut first, 0);
        put_value(&mut first, &ElementProperty::from(1), &mut InlineKeys).unwrap();
        assert_eq!(err.offset, first.len() as u64);
        // Two references to equal strings repeat the key as well.
        let err = read(&encode(&[(0, 1), (2, 2)]), &table, &mut shapes).unwrap_err();
        assert_eq!(err.reason, "duplicate property key");
        // A wide map takes the hashed check.
        let wide_table: Vec<CompactString> = (0..40).map(|i| format!("k{i}").into()).collect();
        let mut entries: Vec<(u64, i64)> = (0..40).map(|i| (i, i as i64)).collect();
        entries.push((7, 0));
        let err = read(&encode(&entries), &wide_table, &mut shapes).unwrap_err();
        assert_eq!(err.reason, "duplicate property key");
        // An index outside the table.
        let err = read(&encode(&[(9, 1)]), &table, &mut shapes).unwrap_err();
        assert!(err.reason.contains("out of range"), "{}", err.reason);
    }
}
