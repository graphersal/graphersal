//! Segment files and their chunks (format spec section 7): the encoder that turns
//! id-sorted elements into self-contained, compressed chunks grouped into size-limited
//! segments, and the decoder that checks and reads them back.
//!
//! The chunk encoder serves the streaming snapshot encoder (`persist::encoder`), which frames
//! its chunks into segments for the packed snapshot (each segment is one section) and the
//! Store's directory snapshots (each segment is one file).

use compact_str::CompactString;
use fnv::FnvHashMap;

use super::codec::{
    self, Codec, DecodeError, DecodeResult, FORMAT_VERSION, KeyWriter, MAX_BLOCK_BYTES, Reader,
    TableKeys, crc, put_properties, put_str, put_u8, put_u32, put_u64, put_uvar,
};
use super::error::PersistError;
use crate::value::PropertyMap;

/// Segment kind byte of vertex segments.
pub(crate) const KIND_VERTEX: u8 = 1;
/// Segment kind byte of edge segments.
pub(crate) const KIND_EDGE: u8 = 2;

/// Magic of a segment file.
pub(crate) const SEGMENT_MAGIC: &[u8; 8] = b"GRSLSEGM";
/// Size of the segment header.
pub(crate) const SEGMENT_HEADER_BYTES: usize = 64;
/// Size of a chunk header: uncompressed len u32, stored len u32, codec u8, element count u32,
/// CRC u32.
pub(crate) const CHUNK_HEADER_BYTES: usize = 17;

/// The default uncompressed chunk target (1 MiB, format spec 7.2).
pub const DEFAULT_CHUNK_BYTES: u32 = 1 << 20;
/// The default segment target (256 MiB).
pub const DEFAULT_SEGMENT_BYTES: u64 = 256 << 20;

/// One chunk's entry in a manifest's chunk index (format spec 6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkIndexEntry {
    pub(crate) offset: u64,
    pub(crate) first_id: CompactString,
    pub(crate) last_id: CompactString,
    pub(crate) crc: u32,
}

/// The per-chunk string table: labels and property/object keys, each once.
#[derive(Default)]
struct StringTable {
    refs: FnvHashMap<CompactString, u64>,
    strings: Vec<CompactString>,
    /// An upper bound of the encoded table size, kept as strings are added.
    bytes: usize,
}

impl StringTable {
    fn reference(&mut self, key: &str) -> u64 {
        if let Some(&index) = self.refs.get(key) {
            return index;
        }
        let index = self.strings.len() as u64;
        let key = CompactString::from(key);
        self.bytes += key.len() + 10;
        self.refs.insert(key.clone(), index);
        self.strings.push(key);
        index
    }

    fn encoded_len(&self) -> usize {
        self.bytes + 10
    }

    fn clear(&mut self) {
        self.refs.clear();
        self.strings.clear();
        self.bytes = 0;
    }
}

impl KeyWriter for StringTable {
    #[inline]
    fn put_key(&mut self, out: &mut Vec<u8>, key: &str) {
        let index = self.reference(key);
        put_uvar(out, index);
    }
}

/// One finished chunk (format spec 7.2): its 17-byte header and stored bytes, ready to be
/// written, and what the chunk index records about it (the offset is its place in a segment,
/// which the caller knows).
#[derive(Debug)]
pub(crate) struct EncodedChunk {
    pub(crate) header: [u8; CHUNK_HEADER_BYTES],
    pub(crate) stored: Vec<u8>,
    pub(crate) first_id: CompactString,
    pub(crate) last_id: CompactString,
    pub(crate) count: u32,
    pub(crate) crc: u32,
}

impl EncodedChunk {
    /// The chunk's size in its segment: header plus stored bytes.
    pub(crate) fn byte_len(&self) -> u64 {
        (CHUNK_HEADER_BYTES + self.stored.len()) as u64
    }
}

/// Encodes elements of one kind, given in strictly increasing id order, into chunks: a chunk
/// closes after the element that brings its uncompressed size to the target and is handed
/// back at once, so the encoder holds one chunk (its body and string table) at a time.
/// Segments are framed around the chunks by the caller ([`segment_header`],
/// [`segment_footer`]).
pub(crate) struct ChunkEncoder {
    kind: u8,
    codec: Codec,
    chunk_target: usize,
    table: StringTable,
    body: Vec<u8>,
    count: u32,
    first: Option<CompactString>,
    last: CompactString,
}

impl ChunkEncoder {
    pub(crate) fn new(kind: u8, codec: Codec, chunk_target: u32) -> Self {
        Self {
            kind,
            codec,
            chunk_target: (chunk_target as usize).clamp(1, MAX_BLOCK_BYTES),
            table: StringTable::default(),
            body: Vec::new(),
            count: 0,
            first: None,
            last: CompactString::default(),
        }
    }

    fn element_error(&self, id: &str, reason: String) -> PersistError {
        let kind = if self.kind == KIND_VERTEX {
            "vertex"
        } else {
            "edge"
        };
        PersistError::encode(format!("{kind} '{id}'"), reason)
    }

    /// Adds a vertex (ids strictly increasing); returns the chunk it closed, if any.
    pub(crate) fn vertex(
        &mut self,
        id: &str,
        labels: &[CompactString],
        properties: &PropertyMap,
    ) -> Result<Option<EncodedChunk>, PersistError> {
        let start = self.body.len();
        put_str(&mut self.body, id);
        put_uvar(&mut self.body, labels.len() as u64);
        for label in labels {
            let index = self.table.reference(label);
            put_uvar(&mut self.body, index);
        }
        if let Err(reason) = put_properties(&mut self.body, Some(properties), &mut self.table) {
            self.body.truncate(start);
            return Err(self.element_error(id, reason));
        }
        self.element_added(id)
    }

    /// Adds an edge (ids strictly increasing); returns the chunk it closed, if any.
    pub(crate) fn edge(
        &mut self,
        id: &str,
        label: Option<&str>,
        out_id: &str,
        in_id: &str,
        properties: Option<&PropertyMap>,
    ) -> Result<Option<EncodedChunk>, PersistError> {
        let start = self.body.len();
        put_str(&mut self.body, id);
        match label {
            None => put_uvar(&mut self.body, 0),
            Some(label) => {
                let index = self.table.reference(label);
                put_uvar(&mut self.body, index + 1);
            }
        }
        put_str(&mut self.body, out_id);
        put_str(&mut self.body, in_id);
        if let Err(reason) = put_properties(&mut self.body, properties, &mut self.table) {
            self.body.truncate(start);
            return Err(self.element_error(id, reason));
        }
        self.element_added(id)
    }

    fn element_added(&mut self, id: &str) -> Result<Option<EncodedChunk>, PersistError> {
        if self.first.is_none() {
            self.first = Some(CompactString::from(id));
        }
        self.last = CompactString::from(id);
        self.count += 1;
        if self.body.len() + self.table.encoded_len() > MAX_BLOCK_BYTES {
            return Err(
                self.element_error(id, format!("a chunk would exceed {MAX_BLOCK_BYTES} bytes"))
            );
        }
        if self.body.len() + self.table.encoded_len() >= self.chunk_target {
            return Ok(self.finish());
        }
        Ok(None)
    }

    /// Closes the current chunk; `None` when it has no element.
    pub(crate) fn finish(&mut self) -> Option<EncodedChunk> {
        let first_id = self.first.take()?;
        let mut payload = Vec::with_capacity(self.body.len() + self.table.encoded_len());
        put_uvar(&mut payload, self.table.strings.len() as u64);
        for string in &self.table.strings {
            put_str(&mut payload, string);
        }
        payload.extend_from_slice(&self.body);
        let uncompressed = payload.len();
        let (codec, compressed) = codec::compress(&payload, self.codec);
        let stored = compressed.unwrap_or(payload);

        let mut header = [0u8; CHUNK_HEADER_BYTES];
        header[0..4].copy_from_slice(&(uncompressed as u32).to_le_bytes());
        header[4..8].copy_from_slice(&(stored.len() as u32).to_le_bytes());
        header[8] = codec;
        header[9..13].copy_from_slice(&self.count.to_le_bytes());
        let chunk_crc = codec::crc_append(crc(&header[..13]), &stored);
        header[13..17].copy_from_slice(&chunk_crc.to_le_bytes());
        let chunk = EncodedChunk {
            header,
            stored,
            first_id,
            last_id: std::mem::take(&mut self.last),
            count: self.count,
            crc: chunk_crc,
        };
        self.table.clear();
        self.body.clear();
        self.count = 0;
        Some(chunk)
    }

    /// The bytes this encoder holds between elements (its buffers' capacities and the string
    /// table): the encoder's own memory accounting.
    pub(crate) fn held_bytes(&self) -> usize {
        self.body.capacity() + self.table.bytes + self.table.strings.capacity() * 24
    }
}

/// The 64-byte header of a segment (format spec 7.1).
pub(crate) fn segment_header(
    kind: u8,
    codec: Codec,
    graph_id: u128,
    chunk_count: u32,
    footer_offset: u64,
) -> [u8; SEGMENT_HEADER_BYTES] {
    let mut header = Vec::with_capacity(SEGMENT_HEADER_BYTES);
    header.extend_from_slice(SEGMENT_MAGIC);
    put_u32(&mut header, FORMAT_VERSION);
    put_u8(&mut header, kind);
    put_u8(&mut header, codec.byte());
    header.extend_from_slice(&[0, 0]);
    header.extend_from_slice(&graph_id.to_be_bytes());
    put_u32(&mut header, chunk_count);
    put_u64(&mut header, footer_offset);
    header.resize(60, 0);
    let header_crc = crc(&header);
    put_u32(&mut header, header_crc);
    let mut out = [0u8; SEGMENT_HEADER_BYTES];
    out.copy_from_slice(&header);
    out
}

/// The footer of a segment (format spec 7.4): every chunk's offset and first id, then the
/// footer's CRC.
pub(crate) fn segment_footer(chunks: &[ChunkIndexEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in chunks {
        put_u64(&mut out, chunk.offset);
        put_str(&mut out, &chunk.first_id);
    }
    let footer_crc = crc(&out);
    put_u32(&mut out, footer_crc);
    out
}

/// A vertex as read from a chunk.
#[derive(Debug)]
pub(crate) struct VertexRecord {
    pub(crate) id: CompactString,
    pub(crate) labels: Vec<CompactString>,
    pub(crate) properties: PropertyMap,
}

/// An edge as read from a chunk.
#[derive(Debug)]
pub(crate) struct EdgeRecord {
    pub(crate) id: CompactString,
    pub(crate) label: Option<CompactString>,
    pub(crate) out_id: CompactString,
    pub(crate) in_id: CompactString,
    pub(crate) properties: PropertyMap,
}

/// One framed, checksum-verified chunk of a segment (not decoded yet).
#[derive(Debug)]
pub(crate) struct RawChunk<'a> {
    pub(crate) offset: u64,
    pub(crate) uncompressed: usize,
    pub(crate) codec: u8,
    pub(crate) count: u32,
    pub(crate) crc: u32,
    pub(crate) stored: &'a [u8],
    /// The file format version of the segment (its header's): the value tags it may hold.
    pub(crate) version: u32,
}

/// The checked framing of a segment: its chunks in order.
#[derive(Debug)]
pub(crate) struct SegmentFrame<'a> {
    pub(crate) chunks: Vec<RawChunk<'a>>,
    /// The footer's first id per chunk.
    pub(crate) footer_ids: Vec<&'a str>,
}

/// The file format version stated by a segment's header bytes (at least its first 64), for a
/// chunk read through the manifest's chunk index without framing the whole segment (a merge
/// checkpoint, a damage scan): the header's version when its checksum holds and this build
/// reads it, else the current one (the chunk's own checksum was checked against the index).
/// An intact header of a NEWER file format version is an error ([`DecodeError::newer`]): its
/// chunks are never decoded as this version's (format spec 15).
pub(crate) fn header_version(head: &[u8]) -> DecodeResult<u32> {
    match head.get(..SEGMENT_HEADER_BYTES) {
        Some(head)
            if head[..8] == SEGMENT_MAGIC[..]
                && crc(&head[..60])
                    == u32::from_le_bytes([head[60], head[61], head[62], head[63]]) =>
        {
            let version = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
            if version > FORMAT_VERSION {
                Err(DecodeError::newer(8, "segment", version))
            } else if codec::readable_version(version) {
                Ok(version)
            } else {
                Ok(FORMAT_VERSION)
            }
        }
        _ => Ok(FORMAT_VERSION),
    }
}

/// Checks a segment's header, chunk framing and checksums and its footer; decodes nothing.
pub(crate) fn frame_segment(
    bytes: &[u8],
    kind: u8,
    graph_id: u128,
) -> DecodeResult<SegmentFrame<'_>> {
    if bytes.len() < SEGMENT_HEADER_BYTES {
        return Err(DecodeError::new(0, "segment shorter than its header"));
    }
    let mut header = Reader::new(&bytes[..SEGMENT_HEADER_BYTES]);
    if header.bytes(8)? != SEGMENT_MAGIC {
        return Err(DecodeError::new(0, "not a segment (bad magic)"));
    }
    let stored_crc = u32::from_le_bytes([bytes[60], bytes[61], bytes[62], bytes[63]]);
    if crc(&bytes[..60]) != stored_crc {
        return Err(DecodeError::new(60, "segment header checksum mismatch"));
    }
    let version = header.u32()?;
    DecodeError::check_version(8, "segment", version)?;
    let found_kind = header.u8()?;
    if found_kind != kind {
        return Err(DecodeError::new(
            12,
            format!("segment kind {found_kind}, expected {kind}"),
        ));
    }
    let _default_codec = header.u8()?;
    header.bytes(2)?;
    let found_graph = header.u128_be()?;
    if found_graph != graph_id {
        return Err(DecodeError::new(16, "segment belongs to another graph"));
    }
    let chunk_count = header.u32()? as usize;
    let footer_offset = header.u64()?;
    if footer_offset < SEGMENT_HEADER_BYTES as u64 || footer_offset > bytes.len() as u64 {
        return Err(DecodeError::new(36, "footer offset out of range"));
    }
    let footer_offset = footer_offset as usize;
    // Every chunk takes at least its header: a larger count is not genuine.
    if chunk_count > (footer_offset - SEGMENT_HEADER_BYTES) / CHUNK_HEADER_BYTES {
        return Err(DecodeError::new(32, "chunk count exceeds the segment size"));
    }

    let mut body = Reader::new(&bytes[..footer_offset]);
    body.bytes(SEGMENT_HEADER_BYTES)?;
    let mut chunks = Vec::with_capacity(chunk_count);
    for _ in 0..chunk_count {
        let offset = body.offset();
        let head = body.bytes(CHUNK_HEADER_BYTES)?;
        let mut head_reader = Reader::new(head);
        let uncompressed = head_reader.u32()? as usize;
        let stored_len = head_reader.u32()? as usize;
        let codec = head_reader.u8()?;
        let count = head_reader.u32()?;
        let chunk_crc = head_reader.u32()?;
        let stored = body
            .bytes(stored_len)
            .map_err(|_| DecodeError::new(offset, "chunk runs past the footer"))?;
        if codec::crc_append(crc(&head[..13]), stored) != chunk_crc {
            return Err(DecodeError::new(offset, "chunk checksum mismatch"));
        }
        chunks.push(RawChunk {
            offset,
            uncompressed,
            codec,
            count,
            crc: chunk_crc,
            stored,
            version,
        });
    }
    if !body.is_empty() {
        return Err(DecodeError::new(
            body.offset(),
            "bytes between the last chunk and the footer",
        ));
    }

    let footer_bytes = &bytes[footer_offset..];
    if footer_bytes.len() < 4 {
        return Err(DecodeError::new(footer_offset as u64, "footer truncated"));
    }
    let (footer_body, footer_crc) = footer_bytes.split_at(footer_bytes.len() - 4);
    if crc(footer_body)
        != u32::from_le_bytes([footer_crc[0], footer_crc[1], footer_crc[2], footer_crc[3]])
    {
        return Err(DecodeError::new(
            footer_offset as u64,
            "footer checksum mismatch",
        ));
    }
    let mut footer = Reader::new(footer_body);
    let mut footer_ids = Vec::with_capacity(chunk_count);
    for chunk in &chunks {
        let at = footer_offset as u64 + footer.offset();
        let offset = footer.u64().map_err(|e| DecodeError::new(at, e.reason))?;
        let first = footer.str().map_err(|e| DecodeError::new(at, e.reason))?;
        if offset != chunk.offset {
            return Err(DecodeError::new(
                at,
                "footer offset differs from the chunk's",
            ));
        }
        footer_ids.push(first);
    }
    if !footer.is_empty() {
        return Err(DecodeError::new(
            footer_offset as u64 + footer.offset(),
            "trailing bytes in the footer",
        ));
    }
    Ok(SegmentFrame { chunks, footer_ids })
}

/// Decompresses a chunk and reads its string table; returns the table and a reader positioned
/// at the first element (over the owned or borrowed payload).
fn open_chunk<'c>(
    chunk: &RawChunk<'c>,
) -> DecodeResult<(std::borrow::Cow<'c, [u8]>, Vec<CompactString>, usize)> {
    let payload = codec::decompress(chunk.codec, chunk.stored, chunk.uncompressed)
        .map_err(|reason| codec::decompress_error(chunk.codec, chunk.offset, reason))?;
    let mut reader = Reader::new(&payload);
    let count = reader.count(1)?;
    let mut table = Vec::with_capacity(count);
    for _ in 0..count {
        table.push(CompactString::from(reader.str()?));
    }
    let position = reader.offset() as usize;
    Ok((payload, table, position))
}

/// Offsets of a chunk-payload error are relative to the payload; report them at the chunk.
fn in_chunk(chunk: &RawChunk<'_>, err: DecodeError) -> DecodeError {
    DecodeError::new(
        chunk.offset,
        format!("{} (payload byte {})", err.reason, err.offset),
    )
}

/// Decodes the vertices of one chunk.
pub(crate) fn decode_vertex_chunk(
    chunk: &RawChunk<'_>,
    max_depth: usize,
) -> DecodeResult<Vec<VertexRecord>> {
    let (payload, table, position) = open_chunk(chunk).map_err(|e| in_chunk(chunk, e))?;
    let keys = TableKeys(&table);
    let mut shapes = codec::ChunkShapes::default();
    let mut reader = Reader::new(&payload[position..]).with_version(chunk.version);
    let count = chunk.count as usize;
    let mut out = Vec::with_capacity(count.min(reader.remaining()));
    let result: DecodeResult<()> = (|| {
        for _ in 0..count {
            let id = CompactString::from(reader.str()?);
            let label_count = reader.count(1)?;
            let mut labels = Vec::with_capacity(label_count);
            for _ in 0..label_count {
                labels.push(keys_label(&keys, &mut reader)?);
            }
            let properties =
                codec::read_element_properties(&mut reader, &table, max_depth, &mut shapes)?;
            out.push(VertexRecord {
                id,
                labels,
                properties,
            });
        }
        if !reader.is_empty() {
            return Err(reader.error("bytes after the chunk's last element"));
        }
        Ok(())
    })();
    result.map_err(|e| in_chunk(chunk, e))?;
    Ok(out)
}

fn keys_label(keys: &TableKeys<'_>, reader: &mut Reader<'_>) -> DecodeResult<CompactString> {
    use super::codec::KeyReader;
    keys.key(reader)
}

/// Decodes the edges of one chunk.
pub(crate) fn decode_edge_chunk(
    chunk: &RawChunk<'_>,
    max_depth: usize,
) -> DecodeResult<Vec<EdgeRecord>> {
    let (payload, table, position) = open_chunk(chunk).map_err(|e| in_chunk(chunk, e))?;
    let mut shapes = codec::ChunkShapes::default();
    let mut reader = Reader::new(&payload[position..]).with_version(chunk.version);
    let count = chunk.count as usize;
    let mut out = Vec::with_capacity(count.min(reader.remaining()));
    let result: DecodeResult<()> = (|| {
        for _ in 0..count {
            let id = CompactString::from(reader.str()?);
            let label_at = reader.offset();
            let label = match reader.uvar()? {
                0 => None,
                reference => Some(
                    usize::try_from(reference - 1)
                        .ok()
                        .and_then(|index| table.get(index))
                        .cloned()
                        .ok_or_else(|| {
                            DecodeError::new(label_at, "label reference out of range")
                        })?,
                ),
            };
            let out_id = CompactString::from(reader.str()?);
            let in_id = CompactString::from(reader.str()?);
            let properties =
                codec::read_element_properties(&mut reader, &table, max_depth, &mut shapes)?;
            out.push(EdgeRecord {
                id,
                label,
                out_id,
                in_id,
                properties,
            });
        }
        if !reader.is_empty() {
            return Err(reader.error("bytes after the chunk's last element"));
        }
        Ok(())
    })();
    result.map_err(|e| in_chunk(chunk, e))?;
    Ok(out)
}
