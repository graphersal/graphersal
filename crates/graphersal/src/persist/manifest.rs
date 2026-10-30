//! The snapshot manifest (format spec section 6): a 4096-byte fixed part with the listing fields,
//! the variable part (meta, segment table, definitions, chunk index), its checksum and a copy
//! of the fixed part at the tail.

use compact_str::CompactString;

use super::codec::{
    DecodeError, DecodeResult, FORMAT_VERSION, FileVersion, Reader, crc, put_str, put_u8, put_u16,
    put_u32, put_u64, put_uvar,
};
use super::definitions::{self, DefinitionRecord};
use super::segment::{ChunkIndexEntry, KIND_EDGE, KIND_VERTEX};

/// Magic of a manifest.
pub(crate) const MANIFEST_MAGIC: &[u8; 8] = b"GRSLMANI";
/// Size of the fixed part.
pub(crate) const FIXED_BYTES: usize = 4096;
/// Offset of the chunk target size in the fixed part (a precision of format spec 6.1: packed
/// snapshots carry the chunk size in the manifest; it sits in the reserved area).
const CHUNK_SIZE_OFFSET: usize = 428;
/// The longest manifest name.
pub(crate) const MAX_NAME_BYTES: usize = 255;
/// Flag bit 0 (format spec 6.1, 6.2): an incremental snapshot, its segment table references
/// files of older snapshots.
pub(crate) const FLAG_INCREMENTAL: u32 = 1;

/// Whether `path` is a well-formed reference to a file of an older snapshot (format spec 6.2):
/// `<20-digit snapshot name>/<file name>`.
pub(crate) fn is_reference(path: &str) -> bool {
    path.split_once('/').is_some_and(|(holder, file)| {
        holder.len() == 20
            && holder.bytes().all(|b| b.is_ascii_digit())
            && !file.is_empty()
            && !file.contains('/')
    })
}

/// The fixed part of a manifest: what a listing needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ManifestFixed {
    pub(crate) flags: u32,
    pub(crate) graph_id: u128,
    pub(crate) parent_graph_id: u128,
    pub(crate) parent_commit_seq: u64,
    pub(crate) commit_seq: u64,
    pub(crate) created_at: i64,
    pub(crate) last_commit_at: i64,
    pub(crate) node_sequence: u64,
    pub(crate) edge_sequence: u64,
    pub(crate) vertex_count: u64,
    pub(crate) edge_count: u64,
    pub(crate) vertex_property_count: u64,
    pub(crate) edge_property_count: u64,
    pub(crate) memory_bytes: u64,
    pub(crate) schema_len: u64,
    pub(crate) schema_crc: u32,
    pub(crate) segment_count: u32,
    pub(crate) variable_offset: u64,
    pub(crate) variable_len: u64,
    pub(crate) name: String,
    pub(crate) chunk_bytes: u32,
    /// The file format version the manifest was read with (not a field of its own: the version
    /// word; [`ManifestFixed::encode`] always writes the current one).
    pub(crate) version: FileVersion,
}

impl ManifestFixed {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FIXED_BYTES);
        out.extend_from_slice(MANIFEST_MAGIC);
        put_u32(&mut out, FORMAT_VERSION);
        put_u32(&mut out, self.flags);
        out.extend_from_slice(&self.graph_id.to_be_bytes());
        out.extend_from_slice(&self.parent_graph_id.to_be_bytes());
        put_u64(&mut out, self.parent_commit_seq);
        put_u64(&mut out, self.commit_seq);
        out.extend_from_slice(&self.created_at.to_le_bytes());
        out.extend_from_slice(&self.last_commit_at.to_le_bytes());
        put_u64(&mut out, self.node_sequence);
        put_u64(&mut out, self.edge_sequence);
        put_u64(&mut out, self.vertex_count);
        put_u64(&mut out, self.edge_count);
        put_u64(&mut out, self.vertex_property_count);
        put_u64(&mut out, self.edge_property_count);
        put_u64(&mut out, self.memory_bytes);
        put_u64(&mut out, self.schema_len);
        put_u32(&mut out, self.schema_crc);
        put_u32(&mut out, self.segment_count);
        put_u64(&mut out, self.variable_offset);
        put_u64(&mut out, self.variable_len);
        let name = truncate_utf8(&self.name, MAX_NAME_BYTES);
        put_u16(&mut out, name.len() as u16);
        let mut name_field = [0u8; MAX_NAME_BYTES];
        name_field[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&name_field);
        out.resize(CHUNK_SIZE_OFFSET, 0);
        put_u32(&mut out, self.chunk_bytes);
        out.resize(FIXED_BYTES - 4, 0);
        let checksum = crc(&out);
        put_u32(&mut out, checksum);
        out
    }

    /// Decodes a 4096-byte fixed part, checksum first.
    pub(crate) fn decode(bytes: &[u8]) -> DecodeResult<Self> {
        if bytes.len() != FIXED_BYTES {
            return Err(DecodeError::new(0, "manifest fixed part truncated"));
        }
        let stored = u32::from_le_bytes([
            bytes[FIXED_BYTES - 4],
            bytes[FIXED_BYTES - 3],
            bytes[FIXED_BYTES - 2],
            bytes[FIXED_BYTES - 1],
        ]);
        if crc(&bytes[..FIXED_BYTES - 4]) != stored {
            return Err(DecodeError::new(
                (FIXED_BYTES - 4) as u64,
                "manifest checksum mismatch",
            ));
        }
        let mut r = Reader::new(bytes);
        if r.bytes(8)? != MANIFEST_MAGIC {
            return Err(DecodeError::new(0, "not a manifest (bad magic)"));
        }
        let version = r.u32()?;
        DecodeError::check_version(8, "manifest", version)?;
        let mut fixed = ManifestFixed {
            flags: r.u32()?,
            graph_id: r.u128_be()?,
            parent_graph_id: r.u128_be()?,
            parent_commit_seq: r.u64()?,
            commit_seq: r.u64()?,
            created_at: r.i64()?,
            last_commit_at: r.i64()?,
            node_sequence: r.u64()?,
            edge_sequence: r.u64()?,
            vertex_count: r.u64()?,
            edge_count: r.u64()?,
            vertex_property_count: r.u64()?,
            edge_property_count: r.u64()?,
            memory_bytes: r.u64()?,
            schema_len: r.u64()?,
            schema_crc: r.u32()?,
            segment_count: r.u32()?,
            variable_offset: r.u64()?,
            variable_len: r.u64()?,
            version: FileVersion(version),
            ..ManifestFixed::default()
        };
        let name_len = r.u16()? as usize;
        if name_len > MAX_NAME_BYTES {
            return Err(DecodeError::new(168, "manifest name longer than 255 bytes"));
        }
        let name = r.bytes(MAX_NAME_BYTES)?;
        fixed.name = std::str::from_utf8(&name[..name_len])
            .map_err(|_| DecodeError::new(170, "manifest name is not UTF-8"))?
            .to_string();
        let mut chunk = Reader::new(&bytes[CHUNK_SIZE_OFFSET..CHUNK_SIZE_OFFSET + 4]);
        fixed.chunk_bytes = chunk.u32()?;
        Ok(fixed)
    }
}

/// `value` cut to at most `max` bytes at a character boundary.
pub(crate) fn truncate_utf8(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// One row of the segment table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentEntry {
    pub(crate) kind: u8,
    pub(crate) path: String,
    pub(crate) first_id: CompactString,
    pub(crate) last_id: CompactString,
    pub(crate) count: u64,
    pub(crate) byte_len: u64,
    pub(crate) crc: u32,
}

/// One row of the chunk index: the segment's number in the table, then the chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkRow {
    pub(crate) segment: u32,
    pub(crate) chunk: ChunkIndexEntry,
}

/// A whole manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub(crate) fixed: ManifestFixed,
    pub(crate) meta: String,
    pub(crate) segments: Vec<SegmentEntry>,
    /// The definitions table (format spec 6.3), payloads not decoded.
    pub(crate) definitions: Vec<DefinitionRecord>,
    pub(crate) chunks: Vec<ChunkRow>,
    /// The blob table (format spec 6.2 item 5, file format version 4): every blob the state
    /// references with its reference count, sorted by `(algorithm, hash)`; empty in a manifest
    /// of an older version.
    pub(crate) blobs: Vec<(crate::value::BlobRef, u64)>,
}

/// Whether a segment kind is one this version reads (1 vertex, 2 edge).
pub(crate) fn is_element_kind(kind: u8) -> bool {
    kind == KIND_VERTEX || kind == KIND_EDGE
}

/// Whether a segment kind this version does not know is critical (format spec 7.5: kinds
/// 128-255; a store whose snapshot has one opens read-only).
pub(crate) fn is_critical_segment_kind(kind: u8) -> bool {
    !is_element_kind(kind) && kind >= 128
}

impl Manifest {
    /// Why a writer of this version must not write the store whose base snapshot this is
    /// (format spec 7.5, 16 rule 3): a file of a critical segment kind it does not know.
    /// Critical definitions are judged on the loaded catalog (the WAL may add or drop them).
    pub(crate) fn critical_segment_reason(&self) -> Option<String> {
        self.segments
            .iter()
            .find(|s| is_critical_segment_kind(s.kind))
            .map(|s| {
                format!(
                    "its snapshot {} holds the file '{}' of the critical segment kind {}, which \
                     this version does not know",
                    self.fixed.commit_seq, s.path, s.kind
                )
            })
    }
}

impl Manifest {
    /// Encodes the manifest; fills `segment_count`, `variable_offset` and `variable_len`.
    pub(crate) fn encode(&mut self) -> Vec<u8> {
        let mut variable = Vec::new();
        put_str(&mut variable, &self.meta);
        for segment in &self.segments {
            put_u8(&mut variable, segment.kind);
            put_str(&mut variable, &segment.path);
            put_str(&mut variable, &segment.first_id);
            put_str(&mut variable, &segment.last_id);
            put_u64(&mut variable, segment.count);
            put_u64(&mut variable, segment.byte_len);
            put_u32(&mut variable, segment.crc);
        }
        definitions::put_table(&mut variable, &self.definitions);
        put_uvar(&mut variable, self.chunks.len() as u64);
        for row in &self.chunks {
            put_u32(&mut variable, row.segment);
            put_u64(&mut variable, row.chunk.offset);
            put_str(&mut variable, &row.chunk.first_id);
            put_str(&mut variable, &row.chunk.last_id);
            put_u32(&mut variable, row.chunk.crc);
        }
        super::store::blobs::put_table(&mut variable, &self.blobs);
        self.fixed.segment_count = self.segments.len() as u32;
        self.fixed.variable_offset = FIXED_BYTES as u64;
        self.fixed.variable_len = variable.len() as u64;
        let fixed = self.fixed.encode();
        let mut out = Vec::with_capacity(2 * FIXED_BYTES + variable.len() + 4);
        out.extend_from_slice(&fixed);
        let variable_crc = crc(&variable);
        out.extend_from_slice(&variable);
        put_u32(&mut out, variable_crc);
        out.extend_from_slice(&fixed);
        out
    }

    /// Decodes a manifest; a damaged head copy of the fixed part falls back to the tail copy.
    pub(crate) fn decode(bytes: &[u8]) -> DecodeResult<Self> {
        if bytes.len() < 2 * FIXED_BYTES + 4 {
            return Err(DecodeError::new(0, "manifest shorter than its fixed parts"));
        }
        let tail_at = bytes.len() - FIXED_BYTES;
        let fixed = match ManifestFixed::decode(&bytes[..FIXED_BYTES]) {
            Ok(fixed) => fixed,
            Err(head) => ManifestFixed::decode(&bytes[tail_at..]).map_err(|_| head)?,
        };
        if fixed.variable_offset != FIXED_BYTES as u64
            || fixed.variable_len != (tail_at - 4 - FIXED_BYTES) as u64
        {
            return Err(DecodeError::new(
                152,
                "variable part position or length mismatch",
            ));
        }
        let variable = &bytes[FIXED_BYTES..tail_at - 4];
        let stored_crc = u32::from_le_bytes([
            bytes[tail_at - 4],
            bytes[tail_at - 3],
            bytes[tail_at - 2],
            bytes[tail_at - 1],
        ]);
        if crc(variable) != stored_crc {
            return Err(DecodeError::new(
                (tail_at - 4) as u64,
                "manifest variable part checksum mismatch",
            ));
        }
        let base = FIXED_BYTES as u64;
        let shift = |e: DecodeError| DecodeError::new(base + e.offset, e.reason);
        let mut r = Reader::new(variable);
        let meta = r.str().map_err(shift)?.to_string();
        let segment_count = fixed.segment_count as usize;
        // Each row takes at least 25 bytes.
        if segment_count > r.remaining() / 25 {
            return Err(DecodeError::new(
                148,
                "segment count exceeds the manifest size",
            ));
        }
        let mut segments = Vec::with_capacity(segment_count);
        let mut seen_edge = false;
        let mut seen_other = false;
        for _ in 0..segment_count {
            let at = base + r.offset();
            let kind = r.u8().map_err(shift)?;
            match kind {
                KIND_VERTEX if seen_edge || seen_other => {
                    return Err(DecodeError::new(at, "vertex segment after an edge segment"));
                }
                KIND_VERTEX => {}
                KIND_EDGE if seen_other => {
                    return Err(DecodeError::new(
                        at,
                        "edge segment after a segment of another kind",
                    ));
                }
                KIND_EDGE => seen_edge = true,
                0 => return Err(DecodeError::new(at, "segment kind 0 is invalid")),
                // Another kind (format spec 7.5): a file of its own, after the element rows.
                _ => seen_other = true,
            }
            segments.push(SegmentEntry {
                kind,
                path: r.str().map_err(shift)?.to_string(),
                first_id: CompactString::from(r.str().map_err(shift)?),
                last_id: CompactString::from(r.str().map_err(shift)?),
                count: r.u64().map_err(shift)?,
                byte_len: r.u64().map_err(shift)?,
                crc: r.u32().map_err(shift)?,
            });
            if let Some(segment) = segments.last()
                && segment.path.contains('/')
            {
                if !is_element_kind(kind) {
                    return Err(DecodeError::new(
                        at,
                        "a file of another segment kind lies outside its snapshot's directory",
                    ));
                }
                if fixed.flags & FLAG_INCREMENTAL == 0 || !is_reference(&segment.path) {
                    return Err(DecodeError::new(
                        at,
                        "a segment path names another snapshot's file, but the manifest is not \
                         incremental or the reference is not `<snapshot>/<file>`",
                    ));
                }
            }
        }
        let definitions = definitions::read_table(&mut r).map_err(shift)?;
        let chunk_count = r.count(18).map_err(shift)?;
        let mut chunks = Vec::with_capacity(chunk_count);
        for _ in 0..chunk_count {
            let at = base + r.offset();
            let segment = r.u32().map_err(shift)?;
            if segment as usize >= segments.len() {
                return Err(DecodeError::new(at, "chunk index names a missing segment"));
            }
            if !is_element_kind(segments[segment as usize].kind) {
                return Err(DecodeError::new(
                    at,
                    "chunk index names a segment that is not a vertex or edge segment",
                ));
            }
            chunks.push(ChunkRow {
                segment,
                chunk: ChunkIndexEntry {
                    offset: r.u64().map_err(shift)?,
                    first_id: CompactString::from(r.str().map_err(shift)?),
                    last_id: CompactString::from(r.str().map_err(shift)?),
                    crc: r.u32().map_err(shift)?,
                },
            });
        }
        let blobs = if fixed.version.0 >= super::codec::BLOB_FORMAT_VERSION {
            let mut table =
                Reader::new(&variable[r.offset() as usize..]).with_version(fixed.version.0);
            let at = r.offset();
            let rows = super::store::blobs::read_table(&mut table)
                .map_err(|e| DecodeError::new(base + at + e.offset, e.reason))?;
            r.bytes(table.offset() as usize).map_err(shift)?;
            rows
        } else {
            Vec::new()
        };
        if !r.is_empty() {
            return Err(DecodeError::new(
                base + r.offset(),
                "trailing bytes in the manifest",
            ));
        }
        Ok(Manifest {
            fixed,
            meta,
            segments,
            definitions,
            chunks,
            blobs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            fixed: ManifestFixed {
                graph_id: 7,
                commit_seq: 42,
                name: "nightly".into(),
                chunk_bytes: 1 << 20,
                ..ManifestFixed::default()
            },
            meta: "{}".into(),
            segments: vec![SegmentEntry {
                kind: KIND_VERTEX,
                path: "v-000000.seg".into(),
                first_id: "a".into(),
                last_id: "z".into(),
                count: 2,
                byte_len: 100,
                crc: 1,
            }],
            definitions: Vec::new(),
            chunks: vec![ChunkRow {
                segment: 0,
                chunk: ChunkIndexEntry {
                    offset: 64,
                    first_id: "a".into(),
                    last_id: "z".into(),
                    crc: 2,
                },
            }],
            blobs: Vec::new(),
        }
    }

    #[test]
    fn a_damaged_head_falls_back_to_the_tail_copy() {
        let mut manifest = sample();
        let bytes = manifest.encode();
        assert_eq!(Manifest::decode(&bytes).unwrap(), manifest);
        let mut damaged = bytes.clone();
        damaged[60] ^= 0x10;
        assert_eq!(Manifest::decode(&damaged).unwrap(), manifest);
        // Both copies damaged: refused.
        let tail = damaged.len() - FIXED_BYTES + 60;
        damaged[tail] ^= 0x10;
        assert!(Manifest::decode(&damaged).is_err());
        // The variable part is checked on its own.
        let mut variable = bytes;
        variable[FIXED_BYTES + 2] ^= 1;
        assert!(Manifest::decode(&variable).is_err());
    }

    #[test]
    fn listing_fields_sit_at_their_fixed_offsets() {
        let mut manifest = sample();
        let bytes = manifest.encode();
        assert_eq!(&bytes[..8], MANIFEST_MAGIC);
        assert_eq!(u64::from_le_bytes(bytes[56..64].try_into().unwrap()), 42);
        assert_eq!(u16::from_le_bytes(bytes[168..170].try_into().unwrap()), 7);
        assert_eq!(&bytes[170..177], b"nightly");
        assert_eq!(
            u32::from_le_bytes(
                bytes[CHUNK_SIZE_OFFSET..CHUNK_SIZE_OFFSET + 4]
                    .try_into()
                    .unwrap()
            ),
            1 << 20
        );
    }
}
