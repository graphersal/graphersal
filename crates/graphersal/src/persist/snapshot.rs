//! The packed single-file snapshot (format spec section 8): writing a graph to any `Write`,
//! reading it back from any `Read` (non-seekable sources included), and reading only its
//! manifest (listing).

use std::io::{Read, Write};

use compact_str::CompactString;

use super::codec::{Codec, DecodeError, FORMAT_VERSION, crc, crc_append};
use super::encoder::{EncodeStats, SegmentOut, SnapshotEncoder};
use super::error::PersistError;
use super::manifest::{
    ChunkRow, FIXED_BYTES, Manifest, ManifestFixed, SegmentEntry, is_element_kind,
};
use super::segment::{
    self, DEFAULT_CHUNK_BYTES, DEFAULT_SEGMENT_BYTES, KIND_EDGE, KIND_VERTEX, RawChunk,
};
use crate::graph::PersistentStorage;
use crate::graph::TraversalGraph;
use crate::graph::schema::GraphSchema;
use crate::value::depth::DEFAULT_MAX_VALUE_DEPTH;

/// Magic of a packed snapshot.
pub(crate) const PACKED_MAGIC: &[u8; 8] = b"GRSLPACK";
/// Section kinds of a packed snapshot (a precision of format spec section 8).
pub(crate) const SECTION_MANIFEST: u8 = 1;
pub(crate) const SECTION_SCHEMA: u8 = 2;
pub(crate) const SECTION_VERTEX_SEGMENT: u8 = 3;
pub(crate) const SECTION_EDGE_SEGMENT: u8 = 4;
/// A named extra file (format spec 8): `name string | the file's bytes`, the packed form of a
/// segment-table row of another kind than vertex or edge (format spec 7.5).
pub(crate) const SECTION_EXTRA_FILE: u8 = 5;

/// The packed section kind of a segment-table row.
pub(crate) fn section_kind(segment_kind: u8) -> u8 {
    match segment_kind {
        KIND_VERTEX => SECTION_VERTEX_SEGMENT,
        KIND_EDGE => SECTION_EDGE_SEGMENT,
        _ => SECTION_EXTRA_FILE,
    }
}

/// The bytes that precede a named extra file's content in its section: the name as `string`.
pub(crate) fn extra_file_prefix(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 2);
    super::codec::put_str(&mut out, name);
    out
}

/// The buffer of a streamed copy or check (64 KiB).
pub(crate) const COPY_BUFFER: usize = 64 << 10;

/// The largest manifest and `schema.json` section a reader accepts (256 MiB each).
const MAX_META_SECTION_BYTES: u64 = 256 << 20;

/// Options of a snapshot write.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WriteOptions {
    pub(crate) name: String,
    pub(crate) meta: String,
    pub(crate) codec: Codec,
    pub(crate) chunk_bytes: u32,
    pub(crate) segment_bytes: u64,
    /// The parent lineage of a fork (`graph_id`, branch `commit_seq`); `(0, 0)` is none.
    pub(crate) parent: (u128, u64),
    /// The manifest's creation time; `None`: now (crate tests fix it to compare bytes).
    pub(crate) created_at: Option<i64>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            meta: String::new(),
            codec: Codec::Lz4,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            segment_bytes: DEFAULT_SEGMENT_BYTES,
            parent: (0, 0),
            created_at: None,
        }
    }
}

impl WriteOptions {
    /// The parent lineage recorded in the manifest (a fork's origin).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn with_parent(mut self, graph_id: u128, commit_seq: u64) -> Self {
        self.parent = (graph_id, commit_seq);
        self
    }

    /// A fixed creation time instead of now (crate tests compare snapshots byte for byte).
    #[cfg(test)]
    pub(crate) fn with_created_at(mut self, micros: i64) -> Self {
        self.created_at = Some(micros);
        self
    }

    /// The defaults: LZ4 chunks of 1 MiB, segments of 256 MiB, no name, no meta.
    pub fn new() -> Self {
        Self::default()
    }

    /// A human-readable name for listings (at most 255 bytes; longer names are cut at a
    /// character boundary).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Free-form JSON text stored with the snapshot and never interpreted (an extension point
    /// for the application).
    pub fn with_meta(mut self, meta: impl Into<String>) -> Self {
        self.meta = meta.into();
        self
    }

    /// The chunk compression ([`Codec::Lz4`] by default).
    pub fn with_codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// The uncompressed chunk target in bytes (default 1 MiB); a chunk is the unit of
    /// decoding and of repair.
    pub fn with_chunk_bytes(mut self, bytes: u32) -> Self {
        self.chunk_bytes = bytes.max(1);
        self
    }

    /// The segment target in bytes (default 256 MiB): a segment closes after the chunk that
    /// reaches it.
    pub fn with_segment_bytes(mut self, bytes: u64) -> Self {
        self.segment_bytes = bytes.max(1);
        self
    }
}

/// Options of a snapshot read.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReadOptions {
    memory_limit: Option<u64>,
    max_value_depth: usize,
    /// Set only for a READ-ONLY load (a Store opened read-only, format spec 16): a graph schema
    /// this version cannot parse is loaded as no schema and noted here instead of failing the
    /// load. Never set where the loaded state may be written anywhere (a lossy view).
    pub(crate) newer_schema: Option<NewerContent>,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            memory_limit: None,
            max_value_depth: DEFAULT_MAX_VALUE_DEPTH,
            newer_schema: None,
        }
    }
}

/// The notes of a read-only load about content a newer version wrote that this one shows
/// leniently ([`ReadOptions::newer_schema`]); shared by the clones of the options.
#[derive(Debug, Clone, Default)]
pub(crate) struct NewerContent(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl NewerContent {
    /// Records one note.
    pub(crate) fn note(&self, note: String) {
        if let Ok(mut notes) = self.0.lock() {
            notes.push(note);
        }
    }

    /// The first note, if any.
    pub(crate) fn first(&self) -> Option<String> {
        self.0.lock().ok().and_then(|notes| notes.first().cloned())
    }
}

/// Parses the schema text of a snapshot or a WAL record (CRC-checked): text this version cannot
/// parse is content of a newer version, never damage. With `newer` (a read-only load) it is
/// loaded as no schema and noted; otherwise it is [`DecodeError::unsupported`].
pub(crate) fn parse_stored_schema(
    text: &str,
    offset: u64,
    newer: Option<&NewerContent>,
) -> Result<GraphSchema, super::codec::DecodeError> {
    match GraphSchema::from_json(text) {
        Ok(schema) => Ok(schema),
        Err(err) => {
            let reason = format!(
                "invalid schema for this version (it uses something this version does not \
                 know: a newer Graphersal wrote it): {err}"
            );
            match newer {
                Some(newer) => {
                    newer.note(format!(
                        "{reason}; it is shown without its schema, read-only"
                    ));
                    Ok(GraphSchema::default())
                }
                None => Err(super::codec::DecodeError::unsupported(offset, reason)),
            }
        }
    }
}

impl ReadOptions {
    /// The defaults: no memory limit, values nested at most 128 levels (the default of
    /// `traversal.max_value_depth`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Refuses a snapshot whose header estimates more than `bytes` of memory for the loaded
    /// graph ([`PersistError::MemoryBudget`]), before any element is read.
    pub fn with_memory_limit(mut self, bytes: u64) -> Self {
        self.memory_limit = Some(bytes);
        self
    }

    /// The deepest value nesting accepted (a graph built through the Rust API with deeper
    /// values needs a higher bound to load).
    pub fn with_max_value_depth(mut self, depth: usize) -> Self {
        self.max_value_depth = depth.max(1);
        self
    }

    pub(crate) fn max_value_depth(&self) -> usize {
        self.max_value_depth
    }
}

/// What a snapshot's manifest says about it: identity, position and counts.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SnapshotInfo {
    /// The lineage id ([`PersistentStorage::graph_id`](crate::graph::PersistentStorage::graph_id)).
    pub graph_id: u128,
    /// The parent lineage id (`0`: none).
    pub parent_graph_id: u128,
    /// The parent's commit sequence number at the branch point (`0`: none).
    pub parent_commit_seq: u64,
    /// The name given at write time (empty: none).
    pub name: String,
    /// The meta JSON text given at write time (empty: none).
    pub meta: String,
    /// The position: every commit up to and including it is contained.
    pub commit_seq: u64,
    /// When the snapshot was written (microseconds since the Unix epoch, UTC).
    pub created_at: i64,
    /// The time of the commit at `commit_seq`.
    pub last_commit_at: i64,
    /// The vertex auto-id sequence.
    pub node_sequence: u64,
    /// The edge auto-id sequence.
    pub edge_sequence: u64,
    /// Vertices.
    pub vertex_count: u64,
    /// Edges.
    pub edge_count: u64,
    /// Top-level vertex properties.
    pub vertex_property_count: u64,
    /// Top-level edge properties.
    pub edge_property_count: u64,
    /// The estimated in-memory size of the loaded graph (`memory_usage()` at write time).
    pub memory_bytes: u64,
    /// The uncompressed chunk target the snapshot was written with.
    pub chunk_bytes: u32,
    /// The number of segments.
    pub segment_count: u32,
}

impl SnapshotInfo {
    pub(crate) fn from_manifest(manifest: &Manifest) -> Self {
        Self {
            meta: manifest.meta.clone(),
            ..Self::from_fixed(&manifest.fixed)
        }
    }

    /// The listing fields of a manifest's fixed part (`meta` is in the variable part: empty).
    pub(crate) fn from_fixed(f: &ManifestFixed) -> Self {
        Self {
            graph_id: f.graph_id,
            parent_graph_id: f.parent_graph_id,
            parent_commit_seq: f.parent_commit_seq,
            name: f.name.clone(),
            meta: String::new(),
            commit_seq: f.commit_seq,
            created_at: f.created_at,
            last_commit_at: f.last_commit_at,
            node_sequence: f.node_sequence,
            edge_sequence: f.edge_sequence,
            vertex_count: f.vertex_count,
            edge_count: f.edge_count,
            vertex_property_count: f.vertex_property_count,
            edge_property_count: f.edge_property_count,
            memory_bytes: f.memory_bytes,
            chunk_bytes: f.chunk_bytes,
            segment_count: f.segment_count,
        }
    }
}

/// A writer that keeps the running CRC-32C of everything written.
struct CrcWriter<W: Write> {
    inner: W,
    crc: u32,
}

impl<W: Write> CrcWriter<W> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), PersistError> {
        self.crc = crc_append(self.crc, bytes);
        self.inner
            .write_all(bytes)
            .map_err(|err| PersistError::io("writing the snapshot", err))
    }

    fn section_head(&mut self, kind: u8, len: u64) -> Result<(), PersistError> {
        self.put(&[kind])?;
        self.put(&len.to_le_bytes())
    }

    fn section(&mut self, kind: u8, bytes: &[u8]) -> Result<(), PersistError> {
        self.section_head(kind, bytes.len() as u64)?;
        self.put(bytes)
    }
}

impl<W: Write> SegmentOut for CrcWriter<W> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), PersistError> {
        CrcWriter::put(self, bytes)
    }
}

/// Writes `graph` as a packed snapshot to `writer` (format spec section 8) and returns what its
/// manifest says. The caller holds the graph's read access for the whole write, so the
/// snapshot is consistent and its position is [`GraphStorage::last_commit_seq`](crate::graph::GraphStorage::last_commit_seq).
///
/// `graph` is any [`PersistentStorage`]: it is read through the storage read API (sorted ids,
/// labels, properties, endpoints), its schema, catalog and position, so the same graph gives
/// the same bytes in every storage.
///
/// The write streams: the manifest, which comes first in the stream, is planned by a first
/// encoding pass that keeps no chunk, and the segments are then encoded again chunk by chunk
/// straight into `writer` (the streaming encoder of format spec 8). Memory: the sorted id
/// index (an id reference and a storage handle per element, 24 bytes for a
/// [`TraversalGraph`]) and one chunk; never the encoded snapshot. `writer` need not seek.
pub fn write_snapshot<S: PersistentStorage + ?Sized, W: Write>(
    graph: &S,
    writer: W,
    options: &WriteOptions,
) -> Result<SnapshotInfo, PersistError> {
    write_packed(graph, writer, options).map(|(info, _)| info)
}

/// [`write_snapshot`], also returning the encoder's own memory figures.
pub(crate) fn write_packed<S: PersistentStorage + ?Sized, W: Write>(
    graph: &S,
    writer: W,
    options: &WriteOptions,
) -> Result<(SnapshotInfo, EncodeStats), PersistError> {
    let encoder = SnapshotEncoder::new(graph, options);
    let mut plans = Vec::new();
    let vertices = encoder.plan_kind(KIND_VERTEX, &mut |plan| {
        plans.push(plan);
        Ok(())
    })?;
    let edges = encoder.plan_kind(KIND_EDGE, &mut |plan| {
        plans.push(plan);
        Ok(())
    })?;
    let schema = encoder.schema();
    let index: Vec<_> = plans.iter().map(|plan| plan.index()).collect();
    let (_, manifest, info) = encoder.manifest(
        &index,
        vertices.property_count,
        edges.property_count,
        &schema,
    )?;
    drop(index);
    let mut out = CrcWriter {
        inner: writer,
        crc: 0,
    };
    let mut head = Vec::with_capacity(16);
    head.extend_from_slice(PACKED_MAGIC);
    head.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    head.extend_from_slice(&(2 + plans.len() as u32).to_le_bytes());
    out.put(&head)?;
    out.section(SECTION_MANIFEST, &manifest)?;
    out.section(SECTION_SCHEMA, &schema)?;
    for plan in &plans {
        out.section_head(section_kind(plan.entry.kind), plan.entry.byte_len)?;
        encoder.emit(plan, &mut out)?;
    }
    let trailer = out.crc.to_le_bytes();
    out.inner
        .write_all(&trailer)
        .and_then(|()| out.inner.flush())
        .map_err(|err| PersistError::io("writing the snapshot", err))?;
    Ok((info, encoder.stats()))
}

/// A reader that keeps the running CRC-32C and the offset of everything read.
struct CrcReader<R: Read> {
    inner: R,
    crc: u32,
    offset: u64,
}

const PACKED: &str = "packed snapshot";

impl<R: Read> CrcReader<R> {
    /// Reads exactly `len` bytes, growing the buffer only as data arrives (a hostile length
    /// allocates nothing up front).
    fn take(&mut self, len: u64, what: &str) -> Result<Vec<u8>, PersistError> {
        let start = self.offset;
        let mut buf = Vec::with_capacity(len.min(1 << 20) as usize);
        (&mut self.inner)
            .take(len)
            .read_to_end(&mut buf)
            .map_err(|err| PersistError::io("reading the snapshot", err))?;
        if (buf.len() as u64) < len {
            return Err(PersistError::corrupt(
                PACKED,
                start,
                format!(
                    "{what} truncated: {len} bytes declared, {} present",
                    buf.len()
                ),
            ));
        }
        self.crc = crc_append(self.crc, &buf);
        self.offset += len;
        Ok(buf)
    }

    /// Copies exactly `len` bytes to `out` through a bounded buffer; returns their CRC.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn copy(&mut self, len: u64, out: &mut dyn Write, what: &str) -> Result<u32, PersistError> {
        let start = self.offset;
        let mut left = len;
        let mut buf = vec![0u8; COPY_BUFFER.min(usize::try_from(len).unwrap_or(usize::MAX))];
        let mut section_crc = 0u32;
        while left > 0 {
            let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let n = match self.inner.read(&mut buf[..want]) {
                Ok(0) => {
                    return Err(PersistError::corrupt(
                        PACKED,
                        start,
                        format!(
                            "{what} truncated: {len} bytes declared, {} present",
                            len - left
                        ),
                    ));
                }
                Ok(n) => n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(PersistError::io("reading the snapshot", err)),
            };
            let bytes = &buf[..n];
            self.crc = crc_append(self.crc, bytes);
            section_crc = crc_append(section_crc, bytes);
            out.write_all(bytes)
                .map_err(|err| PersistError::io("writing a snapshot file", err))?;
            self.offset += n as u64;
            left -= n as u64;
        }
        Ok(section_crc)
    }

    fn fixed<const L: usize>(&mut self, what: &str) -> Result<[u8; L], PersistError> {
        let bytes = self.take(L as u64, what)?;
        let mut out = [0u8; L];
        out.copy_from_slice(&bytes);
        Ok(out)
    }

    /// Reads a section header and its bytes; the kind must be `kind`.
    fn section(&mut self, kind: u8, max: u64, what: &str) -> Result<(u64, Vec<u8>), PersistError> {
        let at = self.offset;
        let [found] = self.fixed::<1>("section header")?;
        if found != kind {
            return Err(PersistError::corrupt(
                PACKED,
                at,
                format!("section kind {found} where the {what} (kind {kind}) belongs"),
            ));
        }
        let len = u64::from_le_bytes(self.fixed::<8>("section header")?);
        if len > max {
            return Err(PersistError::corrupt(
                PACKED,
                at + 1,
                format!("{what} section of {len} bytes exceeds the limit of {max}"),
            ));
        }
        let start = self.offset;
        Ok((start, self.take(len, what)?))
    }
}

impl<R: Read> CrcReader<R> {
    /// Reads the named extra file of the segment-table row `entry` (section kind 5): its name
    /// must be the row's path, its bytes the row's length and CRC. Returns the file's bytes.
    fn extra_file(&mut self, entry: &SegmentEntry) -> Result<Vec<u8>, PersistError> {
        let prefix = extra_file_prefix(&entry.path);
        let (start, bytes) = self.section(
            SECTION_EXTRA_FILE,
            entry.byte_len.saturating_add(prefix.len() as u64),
            "named extra file",
        )?;
        if !bytes.starts_with(&prefix) {
            return Err(PersistError::corrupt(
                PACKED,
                start,
                format!(
                    "the named extra file does not carry the name {:?} of its manifest row",
                    entry.path
                ),
            ));
        }
        let file = bytes[prefix.len()..].to_vec();
        if file.len() as u64 != entry.byte_len || crc(&file) != entry.crc {
            return Err(PersistError::corrupt(
                PACKED,
                start,
                format!(
                    "the named extra file {:?}: length or checksum differs from the manifest",
                    entry.path
                ),
            ));
        }
        Ok(file)
    }
}

/// Reads the packed header and the manifest section.
fn read_head<R: Read>(source: &mut CrcReader<R>) -> Result<(Manifest, u32), PersistError> {
    let magic = source.fixed::<8>("header")?;
    if &magic != PACKED_MAGIC {
        return Err(PersistError::corrupt(
            PACKED,
            0,
            "not a packed snapshot (bad magic)",
        ));
    }
    let version = u32::from_le_bytes(source.fixed::<4>("header")?);
    if !super::codec::readable_version(version) {
        return Err(PersistError::UnsupportedVersion {
            location: PACKED.into(),
            found: version,
            supported: FORMAT_VERSION,
        });
    }
    let sections = u32::from_le_bytes(source.fixed::<4>("header")?);
    let (start, bytes) = source.section(SECTION_MANIFEST, MAX_META_SECTION_BYTES, "manifest")?;
    let manifest = Manifest::decode(&bytes).map_err(|err| err.at("manifest", 0))?;
    let _ = start;
    if manifest.fixed.segment_count as u64 + 2 != u64::from(sections) {
        return Err(PersistError::corrupt(
            PACKED,
            12,
            format!(
                "{sections} sections, but the manifest lists {} segments",
                manifest.fixed.segment_count
            ),
        ));
    }
    Ok((manifest, sections))
}

/// A packed snapshot read section by section into files (a Store imports a packed file as
/// its snapshot directory, format spec 8): the manifest and `schema.json` are checked and kept
/// (each at most 256 MiB), every segment section is copied through a bounded buffer to a
/// writer while its kind, length and CRC are checked against the manifest, and the trailer at
/// the end. The segments' framing is checked where they land (the Store verifies the new
/// directory before it counts).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct PackedReader<R: Read> {
    source: CrcReader<R>,
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) manifest: Manifest,
    pub(crate) schema: Vec<u8>,
    next: usize,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl<R: Read> PackedReader<R> {
    /// Reads the header, the manifest and `schema.json`.
    pub(crate) fn open(reader: R) -> Result<Self, PersistError> {
        let mut source = CrcReader {
            inner: reader,
            crc: 0,
            offset: 0,
        };
        let magic = source.fixed::<8>("header")?;
        if &magic != PACKED_MAGIC {
            return Err(PersistError::corrupt(
                PACKED,
                0,
                "not a packed snapshot (bad magic)",
            ));
        }
        let version = u32::from_le_bytes(source.fixed::<4>("header")?);
        if !super::codec::readable_version(version) {
            return Err(PersistError::UnsupportedVersion {
                location: PACKED.into(),
                found: version,
                supported: FORMAT_VERSION,
            });
        }
        let sections = u32::from_le_bytes(source.fixed::<4>("header")?);
        let (_, manifest_bytes) =
            source.section(SECTION_MANIFEST, MAX_META_SECTION_BYTES, "manifest")?;
        let manifest = Manifest::decode(&manifest_bytes).map_err(|err| err.at("manifest", 0))?;
        if manifest.fixed.segment_count as u64 + 2 != u64::from(sections) {
            return Err(PersistError::corrupt(
                PACKED,
                12,
                "section count differs from the manifest's segment count",
            ));
        }
        let (schema_at, schema) =
            source.section(SECTION_SCHEMA, MAX_META_SECTION_BYTES, "schema.json")?;
        if schema.len() as u64 != manifest.fixed.schema_len
            || crc(&schema) != manifest.fixed.schema_crc
        {
            return Err(PersistError::corrupt(
                "schema.json",
                schema_at,
                "length or checksum differs from the manifest",
            ));
        }
        Ok(Self {
            source,
            manifest_bytes,
            manifest,
            schema,
            next: 0,
        })
    }

    /// Copies the next segment section (in the manifest's table order) to `out`: the file's
    /// bytes (a named extra file without its name). Returns the segment's number.
    pub(crate) fn copy_segment(&mut self, out: &mut dyn Write) -> Result<usize, PersistError> {
        let number = self.next;
        let Some(entry) = self.manifest.segments.get(number) else {
            return Err(PersistError::corrupt(
                PACKED,
                self.source.offset,
                "more segment sections than the manifest lists",
            ));
        };
        let at = self.source.offset;
        let kind = section_kind(entry.kind);
        let [found] = self.source.fixed::<1>("section header")?;
        if found != kind {
            return Err(PersistError::corrupt(
                PACKED,
                at,
                format!("section kind {found} where segment {number} (kind {kind}) belongs"),
            ));
        }
        let len = u64::from_le_bytes(self.source.fixed::<8>("section header")?);
        let prefix = if is_element_kind(entry.kind) {
            Vec::new()
        } else {
            extra_file_prefix(&entry.path)
        };
        if len != entry.byte_len.saturating_add(prefix.len() as u64) {
            return Err(PersistError::corrupt(
                PACKED,
                at + 1,
                format!(
                    "segment {number} ({}): the section's length differs from the manifest",
                    entry.path
                ),
            ));
        }
        if !prefix.is_empty() {
            let start = self.source.offset;
            let name = self.source.take(prefix.len() as u64, "named extra file")?;
            if name != prefix {
                return Err(PersistError::corrupt(
                    PACKED,
                    start,
                    format!(
                        "the named extra file does not carry the name {:?} of its manifest row",
                        entry.path
                    ),
                ));
            }
        }
        let start = self.source.offset;
        let file_crc = self.source.copy(entry.byte_len, out, "segment")?;
        if file_crc != entry.crc {
            return Err(PersistError::corrupt(
                format!("segment {number} ({})", entry.path),
                start,
                "segment length or checksum differs from the manifest",
            ));
        }
        self.next += 1;
        Ok(number)
    }

    /// Checks the trailer (after the last segment).
    pub(crate) fn finish(mut self) -> Result<(), PersistError> {
        if self.next != self.manifest.segments.len() {
            return Err(PersistError::corrupt(
                PACKED,
                self.source.offset,
                "fewer segment sections than the manifest lists",
            ));
        }
        let expected = self.source.crc;
        let trailer_at = self.source.offset;
        let trailer = u32::from_le_bytes(self.source.fixed::<4>("trailer")?);
        if trailer != expected {
            return Err(PersistError::corrupt(
                PACKED,
                trailer_at,
                "trailer checksum mismatch",
            ));
        }
        Ok(())
    }
}

/// Reads only the header and manifest of a packed snapshot: identity, position and counts
/// (a listing), without loading the graph.
pub fn read_snapshot_info<R: Read>(reader: R) -> Result<SnapshotInfo, PersistError> {
    let mut source = CrcReader {
        inner: reader,
        crc: 0,
        offset: 0,
    };
    let (manifest, _) = read_head(&mut source)?;
    Ok(SnapshotInfo::from_manifest(&manifest))
}

/// Decodes every chunk with `decode`, in parallel threads where the platform has them and
/// there is more than one chunk; results in chunk order.
fn decode_chunks<T: Send>(
    chunks: &[RawChunk<'_>],
    decode: impl Fn(&RawChunk<'_>) -> Result<T, DecodeError> + Sync,
) -> Result<Vec<T>, DecodeError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(chunks.len());
        if workers > 1 {
            let per = chunks.len().div_ceil(workers);
            let results: Vec<Result<Vec<T>, DecodeError>> = std::thread::scope(|scope| {
                let handles: Vec<_> = chunks
                    .chunks(per)
                    .map(|group| {
                        let decode = &decode;
                        scope.spawn(move || group.iter().map(decode).collect::<Result<Vec<T>, _>>())
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle.join().unwrap_or_else(|_| {
                            Err(DecodeError::new(0, "a decoder thread panicked"))
                        })
                    })
                    .collect()
            });
            let mut out = Vec::with_capacity(chunks.len());
            for group in results {
                out.extend(group?);
            }
            return Ok(out);
        }
    }
    chunks.iter().map(decode).collect()
}

/// Checks one segment section against its manifest row and chunk index, and frames it.
pub(crate) fn check_segment<'b>(
    manifest: &Manifest,
    number: usize,
    bytes: &'b [u8],
    location: &str,
) -> Result<segment::SegmentFrame<'b>, PersistError> {
    let entry = &manifest.segments[number];
    if bytes.len() as u64 != entry.byte_len || crc(bytes) != entry.crc {
        return Err(PersistError::corrupt(
            location,
            0,
            "segment length or checksum differs from the manifest",
        ));
    }
    let frame = segment::frame_segment(bytes, entry.kind, manifest.fixed.graph_id)
        .map_err(|err| err.at(location, 0))?;
    let rows: Vec<&ChunkRow> = manifest
        .chunks
        .iter()
        .filter(|row| row.segment as usize == number)
        .collect();
    if rows.len() != frame.chunks.len() {
        return Err(PersistError::corrupt(
            location,
            0,
            "chunk count differs from the manifest's chunk index",
        ));
    }
    for ((row, chunk), footer_id) in rows.iter().zip(&frame.chunks).zip(&frame.footer_ids) {
        if row.chunk.offset != chunk.offset
            || row.chunk.crc != chunk.crc
            || row.chunk.first_id.as_str() != *footer_id
        {
            return Err(PersistError::corrupt(
                location,
                chunk.offset,
                "chunk differs from the manifest's chunk index",
            ));
        }
    }
    Ok(frame)
}

/// A failure of [`check_segment_stream`]'s framing: the source failed, the bytes are wrong, or
/// the caller's chunk handler failed.
enum StreamFault {
    Io(std::io::Error),
    Bad(DecodeError),
    Handler(PersistError),
}

impl From<DecodeError> for StreamFault {
    fn from(err: DecodeError) -> Self {
        StreamFault::Bad(err)
    }
}

/// A segment file read sequentially, with its running CRC and offset.
struct SegmentStream<'r> {
    inner: &'r mut dyn Read,
    crc: u32,
    offset: u64,
}

impl SegmentStream<'_> {
    /// Reads exactly `len` bytes into `buf` (replacing its contents), growing it only as data
    /// arrives; fewer bytes are a truncation at the current offset.
    fn read(&mut self, len: u64, buf: &mut Vec<u8>, what: &str) -> Result<(), StreamFault> {
        buf.clear();
        let at = self.offset;
        (&mut *self.inner)
            .take(len)
            .read_to_end(buf)
            .map_err(StreamFault::Io)?;
        self.crc = crc_append(self.crc, buf);
        self.offset += buf.len() as u64;
        if (buf.len() as u64) < len {
            return Err(StreamFault::Bad(DecodeError::new(
                at,
                format!("{what} truncated"),
            )));
        }
        Ok(())
    }

    /// Reads on to the end, at most `limit` more bytes (the CRC and offset keep counting).
    fn drain(&mut self, limit: u64) -> std::io::Result<()> {
        let mut buf = vec![0u8; COPY_BUFFER];
        let mut left = limit;
        while left > 0 {
            let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let n = match self.inner.read(&mut buf[..want]) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            };
            self.crc = crc_append(self.crc, &buf[..n]);
            self.offset += n as u64;
            left -= n as u64;
        }
        Ok(())
    }
}

/// [`check_segment`] over a sequential reader of the segment file, holding one chunk at a
/// time: the file's length and CRC against the manifest row (reported first, as there), the
/// header, every chunk's framing and CRC against the chunk index, and the footer. Decodes no
/// element.
pub(crate) fn check_segment_stream(
    manifest: &Manifest,
    number: usize,
    reader: &mut dyn Read,
    location: &str,
) -> Result<(), PersistError> {
    read_segment_stream(manifest, number, reader, location, &mut |_, _| Ok(()))
}

/// The handler of [`read_segment_stream`]: a verified chunk with its row of the manifest's
/// chunk index (its framing checked, not decoded).
pub(crate) type ChunkHandler<'h> =
    dyn FnMut(&ChunkRow, &RawChunk<'_>) -> Result<(), PersistError> + 'h;

/// [`check_segment_stream`] that hands every chunk, once its CRC and its chunk index row are
/// checked, to `on_chunk` (which may decode it: a merge checkpoint reads a base segment this
/// way, one chunk at a time). A failure of the handler stops the read and is returned as it
/// is. The whole file's length and CRC are checked at the end, so a handler sees chunks of a
/// file that may still turn out damaged: whatever it built must stay invisible until the read
/// returned `Ok`.
pub(crate) fn read_segment_stream(
    manifest: &Manifest,
    number: usize,
    reader: &mut dyn Read,
    location: &str,
    on_chunk: &mut ChunkHandler<'_>,
) -> Result<(), PersistError> {
    let entry = &manifest.segments[number];
    let mut stream = SegmentStream {
        inner: reader,
        crc: 0,
        offset: 0,
    };
    let framed = frame_segment_stream(manifest, number, &mut stream, on_chunk);
    match framed {
        Err(StreamFault::Io(err)) => {
            return Err(PersistError::io("reading a snapshot segment", err));
        }
        Err(StreamFault::Handler(err)) => return Err(err),
        _ => {}
    }
    // Whatever the framing found, the whole file's length and CRC decide first.
    let rest = (entry.byte_len + 1).saturating_sub(stream.offset);
    stream
        .drain(rest)
        .map_err(|err| PersistError::io("reading a snapshot segment", err))?;
    if stream.offset != entry.byte_len || stream.crc != entry.crc {
        return Err(PersistError::corrupt(
            location,
            0,
            "segment length or checksum differs from the manifest",
        ));
    }
    match framed {
        Ok(()) => Ok(()),
        Err(StreamFault::Bad(err)) => Err(err.at(location, 0)),
        Err(StreamFault::Io(err)) => Err(PersistError::io("reading a snapshot segment", err)),
        Err(StreamFault::Handler(err)) => Err(err),
    }
}

/// The framing checks of [`check_segment_stream`] (the same as [`segment::frame_segment`] and
/// [`check_segment`]).
fn frame_segment_stream(
    manifest: &Manifest,
    number: usize,
    stream: &mut SegmentStream<'_>,
    on_chunk: &mut ChunkHandler<'_>,
) -> Result<(), StreamFault> {
    use segment::{CHUNK_HEADER_BYTES, SEGMENT_HEADER_BYTES, SEGMENT_MAGIC};
    let entry = &manifest.segments[number];
    let mut buf = Vec::new();
    stream
        .read(SEGMENT_HEADER_BYTES as u64, &mut buf, "segment header")
        .map_err(|fault| match fault {
            StreamFault::Bad(_) => DecodeError::new(0, "segment shorter than its header").into(),
            io => io,
        })?;
    if &buf[..8] != SEGMENT_MAGIC {
        return Err(DecodeError::new(0, "not a segment (bad magic)").into());
    }
    if crc(&buf[..60]) != u32::from_le_bytes([buf[60], buf[61], buf[62], buf[63]]) {
        return Err(DecodeError::new(60, "segment header checksum mismatch").into());
    }
    let mut header = super::codec::Reader::new(&buf[8..60]);
    let version = header.u32()?;
    DecodeError::check_version(8, "segment", version)?;
    let found_kind = header.u8()?;
    if found_kind != entry.kind {
        return Err(DecodeError::new(
            12,
            format!("segment kind {found_kind}, expected {}", entry.kind),
        )
        .into());
    }
    let _default_codec = header.u8()?;
    header.bytes(2)?;
    if header.u128_be()? != manifest.fixed.graph_id {
        return Err(DecodeError::new(16, "segment belongs to another graph").into());
    }
    let chunk_count = header.u32()? as usize;
    let footer_offset = header.u64()?;
    if footer_offset < SEGMENT_HEADER_BYTES as u64 || footer_offset > entry.byte_len {
        return Err(DecodeError::new(36, "footer offset out of range").into());
    }
    if chunk_count as u64
        > (footer_offset - SEGMENT_HEADER_BYTES as u64) / CHUNK_HEADER_BYTES as u64
    {
        return Err(DecodeError::new(32, "chunk count exceeds the segment size").into());
    }
    let rows: Vec<&ChunkRow> = manifest
        .chunks
        .iter()
        .filter(|row| row.segment as usize == number)
        .collect();
    if rows.len() != chunk_count {
        return Err(
            DecodeError::new(0, "chunk count differs from the manifest's chunk index").into(),
        );
    }
    for row in &rows {
        let offset = stream.offset;
        if offset + CHUNK_HEADER_BYTES as u64 > footer_offset {
            return Err(DecodeError::new(offset, "chunk runs past the footer").into());
        }
        stream.read(CHUNK_HEADER_BYTES as u64, &mut buf, "chunk header")?;
        let head: [u8; CHUNK_HEADER_BYTES] = buf[..]
            .try_into()
            .map_err(|_| StreamFault::Bad(DecodeError::new(offset, "chunk header truncated")))?;
        let stored_len = u64::from(u32::from_le_bytes([head[4], head[5], head[6], head[7]]));
        let chunk_crc = u32::from_le_bytes([head[13], head[14], head[15], head[16]]);
        if stream.offset + stored_len > footer_offset {
            return Err(DecodeError::new(offset, "chunk runs past the footer").into());
        }
        stream.read(stored_len, &mut buf, "chunk")?;
        if crc_append(crc(&head[..13]), &buf) != chunk_crc {
            return Err(DecodeError::new(offset, "chunk checksum mismatch").into());
        }
        if row.chunk.offset != offset || row.chunk.crc != chunk_crc {
            return Err(
                DecodeError::new(offset, "chunk differs from the manifest's chunk index").into(),
            );
        }
        let chunk = RawChunk {
            offset,
            uncompressed: u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize,
            codec: head[8],
            count: u32::from_le_bytes([head[9], head[10], head[11], head[12]]),
            crc: chunk_crc,
            stored: &buf,
            version,
        };
        on_chunk(row, &chunk).map_err(StreamFault::Handler)?;
    }
    if stream.offset != footer_offset {
        return Err(
            DecodeError::new(stream.offset, "bytes between the last chunk and the footer").into(),
        );
    }
    stream.read(entry.byte_len - footer_offset, &mut buf, "footer")?;
    if buf.len() < 4 {
        return Err(DecodeError::new(footer_offset, "footer truncated").into());
    }
    let (footer_body, footer_crc) = buf.split_at(buf.len() - 4);
    if crc(footer_body)
        != u32::from_le_bytes([footer_crc[0], footer_crc[1], footer_crc[2], footer_crc[3]])
    {
        return Err(DecodeError::new(footer_offset, "footer checksum mismatch").into());
    }
    let mut footer = super::codec::Reader::new(footer_body);
    for row in &rows {
        let at = footer_offset + footer.offset();
        let offset = footer.u64().map_err(|e| DecodeError::new(at, e.reason))?;
        let first = footer.str().map_err(|e| DecodeError::new(at, e.reason))?;
        if offset != row.chunk.offset {
            return Err(DecodeError::new(at, "footer offset differs from the chunk's").into());
        }
        if first != row.chunk.first_id.as_str() {
            return Err(DecodeError::new(
                row.chunk.offset,
                "chunk differs from the manifest's chunk index",
            )
            .into());
        }
    }
    if !footer.is_empty() {
        return Err(DecodeError::new(
            footer_offset + footer.offset(),
            "trailing bytes in the footer",
        )
        .into());
    }
    Ok(())
}

/// Checks a decoded chunk's ids against the index row and the running order.
pub(crate) fn check_ids<'i>(
    ids: impl Iterator<Item = &'i CompactString>,
    row: &ChunkRow,
    last: &mut Option<CompactString>,
    location: &str,
    offset: u64,
) -> Result<(), PersistError> {
    let mut first = None;
    let mut current: Option<&CompactString> = None;
    for id in ids {
        if let Some(previous) = current.or(last.as_ref())
            && id.as_bytes() <= previous.as_bytes()
        {
            return Err(PersistError::corrupt(
                location,
                offset,
                format!("element ids out of order at '{id}'"),
            ));
        }
        if first.is_none() {
            first = Some(id);
        }
        current = Some(id);
    }
    match (first, current) {
        (Some(first), Some(end)) if *first == row.chunk.first_id && *end == row.chunk.last_id => {
            *last = Some(end.clone());
            Ok(())
        }
        _ => Err(PersistError::corrupt(
            location,
            offset,
            "chunk id range differs from the manifest's chunk index",
        )),
    }
}

/// Reads a packed snapshot into a new graph (format spec sections 8 and 12.1): vertices, then
/// edges (endpoints resolved by id), no schema validation (integrity comes from the
/// checksums), statistics and indexes rebuilt, the lineage id, commit position, commit time
/// and auto-id sequences restored. Every byte is checked; damage is a
/// [`PersistError::Corrupt`] naming the section and offset.
///
/// The non-generic form of [`read_snapshot_into`] for a [`TraversalGraph`].
pub fn read_snapshot<R: Read>(
    reader: R,
    options: &ReadOptions,
) -> Result<TraversalGraph, PersistError> {
    read_snapshot_into(reader, options, TraversalGraph::new())
}

/// Reads a packed snapshot into `graph`, an EMPTY storage of any [`PersistentStorage`] type
/// (`S::default()`, a new instance, or one emptied by [`PersistentStorage::clear`]), and returns
/// it: the same checks as [`read_snapshot`]; the elements, schema and catalog go through the
/// storage's load sink ([`PersistentStorage::load_schema`], `load_definition`, `load_vertex`,
/// `load_edge`, then `finish_load` once), then the lineage id, the commit position and time
/// and the auto-id sequences are restored ([`PersistentStorage::set_graph_id`],
/// [`PersistentStorage::raise_position`], [`GraphStorage::raise_id_sequences`](crate::graph::GraphStorage::raise_id_sequences)). The chunks
/// are decoded in parallel off wasm32.
///
/// A storage that already holds elements fails with [`PersistError::Graph`] holding
/// [`GraphError::Unsupported`](crate::error::GraphError::Unsupported) before anything is read.
///
/// ```rust
/// use graphersal::persist::{ReadOptions, read_snapshot_into, write_snapshot, WriteOptions};
/// use graphersal::prelude::*;
///
/// let graph = TraversalGraph::tinkerpop_modern();
/// let mut bytes = Vec::new();
/// write_snapshot(&graph, &mut bytes, &WriteOptions::new()).unwrap();
/// let loaded = read_snapshot_into(bytes.as_slice(), &ReadOptions::new(), TraversalGraph::new())
///     .unwrap();
/// assert_eq!(loaded.vertex_count(), 6);
/// ```
pub fn read_snapshot_into<S: PersistentStorage, R: Read>(
    reader: R,
    options: &ReadOptions,
    mut graph: S,
) -> Result<S, PersistError> {
    load_snapshot(reader, options, &mut graph)?;
    Ok(graph)
}

/// [`read_snapshot_into`] into a borrowed empty storage (a reload in place keeps the instance
/// and its hooks). On error the storage is half-loaded: clear or drop it.
pub(crate) fn load_snapshot<S: PersistentStorage + ?Sized, R: Read>(
    reader: R,
    options: &ReadOptions,
    graph: &mut S,
) -> Result<(), PersistError> {
    if graph.vertex_count() != 0 || graph.edge_count() != 0 {
        return Err(PersistError::Graph(crate::error::GraphError::unsupported(
            "loading a snapshot into a storage that holds elements",
        )));
    }
    let mut source = CrcReader {
        inner: reader,
        crc: 0,
        offset: 0,
    };
    let (manifest, _) = read_head(&mut source)?;
    let fixed = &manifest.fixed;
    if let Some(limit) = options.memory_limit
        && fixed.memory_bytes > limit
    {
        return Err(PersistError::MemoryBudget {
            needed: fixed.memory_bytes,
            limit,
        });
    }

    let (schema_at, schema_bytes) =
        source.section(SECTION_SCHEMA, MAX_META_SECTION_BYTES, "schema.json")?;
    if schema_bytes.len() as u64 != fixed.schema_len || crc(&schema_bytes) != fixed.schema_crc {
        return Err(PersistError::corrupt(
            "schema.json",
            schema_at,
            "length or checksum differs from the manifest",
        ));
    }
    let schema_text = std::str::from_utf8(&schema_bytes)
        .map_err(|_| PersistError::corrupt("schema.json", schema_at, "not UTF-8"))?;
    let schema = parse_stored_schema(schema_text, 0, options.newer_schema.as_ref())
        .map_err(|err| err.at("schema.json", schema_at))?;

    graph.load_schema(schema)?;
    // Allocate the slots once when the counts agree with the manifest's own memory estimate (a
    // damaged manifest claiming billions of elements reserves nothing; the load then fails on
    // the count check below).
    let claimed_slots = fixed
        .vertex_count
        .saturating_add(fixed.edge_count)
        .saturating_mul(64);
    if claimed_slots <= fixed.memory_bytes {
        graph.reserve_load(fixed.vertex_count, fixed.edge_count);
    }
    let max_depth = options.max_value_depth;
    restore_definitions(graph, &manifest, max_depth)?;

    let mut last_vertex: Option<CompactString> = None;
    let mut last_edge: Option<CompactString> = None;
    let (mut vertex_properties, mut edge_properties) = (0u64, 0u64);
    for number in 0..manifest.segments.len() {
        let entry = &manifest.segments[number];
        if !is_element_kind(entry.kind) {
            // A file of another kind (format spec 7.5): checked, not interpreted here.
            source.extra_file(entry)?;
            continue;
        }
        let (section, kind_name) = if entry.kind == KIND_VERTEX {
            (SECTION_VERTEX_SEGMENT, "vertex segment")
        } else {
            (SECTION_EDGE_SEGMENT, "edge segment")
        };
        let (_, bytes) = source.section(section, entry.byte_len, kind_name)?;
        let location = format!("{kind_name} {number} ({})", entry.path);
        let frame = check_segment(&manifest, number, &bytes, &location)?;
        let rows: Vec<&ChunkRow> = manifest
            .chunks
            .iter()
            .filter(|row| row.segment as usize == number)
            .collect();
        let mut count = 0u64;
        if entry.kind == KIND_VERTEX {
            let decoded = decode_chunks(&frame.chunks, |chunk| {
                segment::decode_vertex_chunk(chunk, max_depth)
            })
            .map_err(|err| err.at(&location, 0))?;
            for ((records, row), chunk) in decoded.into_iter().zip(&rows).zip(&frame.chunks) {
                check_ids(
                    records.iter().map(|r| &r.id),
                    row,
                    &mut last_vertex,
                    &location,
                    chunk.offset,
                )?;
                for record in records {
                    for (i, label) in record.labels.iter().enumerate() {
                        if record.labels[..i].contains(label) {
                            return Err(PersistError::corrupt(
                                &location,
                                chunk.offset,
                                format!("vertex '{}' repeats the label '{label}'", record.id),
                            ));
                        }
                    }
                    vertex_properties += record.properties.len() as u64;
                    count += 1;
                    graph
                        .load_vertex(record.id, record.labels, record.properties)
                        .map_err(|err| {
                            PersistError::corrupt(&location, chunk.offset, err.to_string())
                        })?;
                }
            }
        } else {
            let decoded = decode_chunks(&frame.chunks, |chunk| {
                segment::decode_edge_chunk(chunk, max_depth)
            })
            .map_err(|err| err.at(&location, 0))?;
            for ((records, row), chunk) in decoded.into_iter().zip(&rows).zip(&frame.chunks) {
                check_ids(
                    records.iter().map(|r| &r.id),
                    row,
                    &mut last_edge,
                    &location,
                    chunk.offset,
                )?;
                for record in records {
                    edge_properties += record.properties.len() as u64;
                    count += 1;
                    let id = record.id.clone();
                    graph
                        .load_edge(
                            record.id,
                            record.label,
                            &record.out_id,
                            &record.in_id,
                            record.properties,
                        )
                        .map_err(|err| match err {
                            crate::error::GraphError::VertexNotFound { .. } => {
                                PersistError::corrupt(
                                    &location,
                                    chunk.offset,
                                    format!("edge '{id}' names a missing endpoint"),
                                )
                            }
                            other => {
                                PersistError::corrupt(&location, chunk.offset, other.to_string())
                            }
                        })?;
                }
            }
        }
        if count != entry.count
            || rows.first().map(|r| &r.chunk.first_id) != Some(&entry.first_id)
            || rows.last().map(|r| &r.chunk.last_id) != Some(&entry.last_id)
        {
            return Err(PersistError::corrupt(
                &location,
                0,
                "element count or id range differs from the manifest",
            ));
        }
    }
    if graph.vertex_count() as u64 != fixed.vertex_count
        || graph.edge_count() as u64 != fixed.edge_count
        || vertex_properties != fixed.vertex_property_count
        || edge_properties != fixed.edge_property_count
    {
        return Err(PersistError::corrupt(
            "manifest",
            96,
            "element or property counts differ from the segments",
        ));
    }
    let expected = source.crc;
    let trailer_at = source.offset;
    let trailer = u32::from_le_bytes(source.fixed::<4>("trailer")?);
    if trailer != expected {
        return Err(PersistError::corrupt(
            PACKED,
            trailer_at,
            "trailer checksum mismatch",
        ));
    }
    let _ = FIXED_BYTES;
    // Derived state (a `TraversalGraph`'s compressed properties, D11: the file holds plain
    // values; the end of the load encodes the rule paths and builds the dictionaries).
    graph.finish_load();
    graph.set_graph_id(fixed.graph_id);
    graph.raise_position(fixed.commit_seq, fixed.last_commit_at);
    graph.raise_id_sequences(fixed.node_sequence, fixed.edge_sequence)?;
    Ok(())
}

/// Puts the manifest's definitions into `graph`'s catalog (no validation: they were valid when
/// committed; an unknown kind stays opaque).
pub(crate) fn restore_definitions<S: PersistentStorage + ?Sized>(
    graph: &mut S,
    manifest: &Manifest,
    max_depth: usize,
) -> Result<(), PersistError> {
    for record in &manifest.definitions {
        // A payload whose fields this version cannot read is kept (opaque or not served); a
        // payload that is not a valid property map is damage (format spec 6.3, 16).
        let definition = record
            .decode_in(max_depth, manifest.fixed.version.0)
            .map_err(|err| {
                PersistError::corrupt(
                    "manifest",
                    FIXED_BYTES as u64,
                    format!(
                        "definition '{}' of kind {}: {} (payload byte {})",
                        record.name, record.kind, err.reason, err.offset
                    ),
                )
            })?;
        graph.load_definition(definition)?;
    }
    Ok(())
}

impl TraversalGraph {
    /// Writes this graph as a packed snapshot with the default [`WriteOptions`]; see
    /// [`write_snapshot`].
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    /// use graphersal::storage::PersistentStorage;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let mut bytes = Vec::new();
    /// graph.write_snapshot(&mut bytes).unwrap();
    /// let loaded = TraversalGraph::read_snapshot(bytes.as_slice()).unwrap();
    /// assert_eq!(loaded.vertex_count(), 6);
    /// assert_eq!(loaded.graph_id(), graph.graph_id());
    /// ```
    pub fn write_snapshot<W: Write>(&self, writer: W) -> Result<SnapshotInfo, PersistError> {
        write_snapshot(self, writer, &WriteOptions::default())
    }

    /// Reads a packed snapshot with the default [`ReadOptions`]; see [`read_snapshot`].
    pub fn read_snapshot<R: Read>(reader: R) -> Result<TraversalGraph, PersistError> {
        read_snapshot(reader, &ReadOptions::default())
    }
}
