//! Journal (write-ahead log) records over streams (format spec section 9): the
//! segment header, the record frame with its sync marker, the lossless commit payload of a
//! [`ChangeSet`], marks, and [`JournalReader`], which reads them back and tells a torn tail
//! (an incomplete last write) from damage.

use std::io::Read;

use super::codec::{
    self, COMMIT_PARTS_FORMAT_VERSION, Codec, DecodeError, DecodeResult, FORMAT_VERSION,
    InlineKeys, MAX_BLOCK_BYTES, Reader, crc, put_i64, put_opt_str, put_properties, put_str,
    put_u8, put_u32, put_u64, put_uvar, put_value, read_properties, read_value, tag,
};
use super::definitions::DefinitionRecord;
use super::error::PersistError;
use super::snapshot::NewerContent;
use crate::graph::change::{
    ChangeSet, CommitMetadata, CommitStamp, EdgeState, ElementRef, Mutation, VertexState,
};
use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::hooks::Mark;
use crate::graph::schema::{ElementKind, GraphSchema};

/// Magic of a journal segment.
pub(crate) const WAL_MAGIC: &[u8; 8] = b"GRSLWALS";
/// Size of the journal segment header.
pub(crate) const WAL_HEADER_BYTES: usize = 64;
/// The sync marker that starts every record ("GRSW").
pub(crate) const SYNC_MARKER: u32 = 0x4752_5357;
/// Size of a record frame header: marker, length, commit_seq, type, header CRC, body CRC.
pub(crate) const FRAME_BYTES: usize = 25;

/// Record types.
pub(crate) const RECORD_COMMIT: u8 = 1;
pub(crate) const RECORD_MARK: u8 = 2;
pub(crate) const RECORD_CHECKPOINT: u8 = 3;
/// A part of a commit written in parts (format spec 9.3.1; file format 4).
pub(crate) const RECORD_COMMIT_PART: u8 = 4;

/// The header of a journal segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WalHeader {
    pub(crate) graph_id: u128,
    pub(crate) first_commit_seq: u64,
    pub(crate) created_at: i64,
}

impl WalHeader {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(WAL_HEADER_BYTES);
        out.extend_from_slice(WAL_MAGIC);
        put_u32(&mut out, FORMAT_VERSION);
        out.extend_from_slice(&self.graph_id.to_be_bytes());
        put_u64(&mut out, self.first_commit_seq);
        put_i64(&mut out, self.created_at);
        out.resize(60, 0);
        let checksum = crc(&out);
        put_u32(&mut out, checksum);
        out
    }

    pub(crate) fn decode(bytes: &[u8]) -> DecodeResult<Self> {
        Self::decode_versioned(bytes).map(|(header, _)| header)
    }

    /// The file format version of a journal segment's bytes for reading its records: the
    /// header's, or the current one when the header is damaged (salvage reads the records
    /// without it).
    pub(crate) fn records_version(segment: &[u8]) -> u32 {
        segment
            .get(..WAL_HEADER_BYTES)
            .and_then(|head| Self::decode_versioned(head).ok())
            .map_or(FORMAT_VERSION, |(_, version)| version)
    }

    /// The header and the file format version it states (1 or 2).
    pub(crate) fn decode_versioned(bytes: &[u8]) -> DecodeResult<(Self, u32)> {
        if bytes.len() != WAL_HEADER_BYTES {
            return Err(DecodeError::new(0, "journal header truncated"));
        }
        if &bytes[..8] != WAL_MAGIC {
            return Err(DecodeError::new(0, "not a journal (bad magic)"));
        }
        if crc(&bytes[..60]) != u32::from_le_bytes([bytes[60], bytes[61], bytes[62], bytes[63]]) {
            return Err(DecodeError::new(60, "journal header checksum mismatch"));
        }
        let mut r = Reader::new(&bytes[8..60]);
        let version = r.u32()?;
        DecodeError::check_version(8, "journal", version)?;
        let header = WalHeader {
            graph_id: r.u128_be()?,
            first_commit_seq: r.u64()?,
            created_at: r.i64()?,
        };
        Ok((header, version))
    }
}

/// Frames one record (format spec 9.2): sync marker, payload length, commit_seq, type, the
/// CRC-32C of these 17 header bytes, the CRC-32C of the payload, then the payload.
pub(crate) fn frame_record(
    out: &mut Vec<u8>,
    record_type: u8,
    commit_seq: u64,
    payload: &[u8],
) -> Result<(), PersistError> {
    let length = u32::try_from(payload.len()).map_err(|_| {
        PersistError::encode(
            format!("journal record of commit {commit_seq}"),
            "larger than 4 GiB",
        )
    })?;
    let start = out.len();
    put_u32(out, SYNC_MARKER);
    put_u32(out, length);
    put_u64(out, commit_seq);
    put_u8(out, record_type);
    let header_crc = crc(&out[start..]);
    put_u32(out, header_crc);
    put_u32(out, crc(payload));
    out.extend_from_slice(payload);
    Ok(())
}

/// A decoded record frame header (the first [`FRAME_BYTES`] bytes of a record).
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameHeader {
    pub(crate) length: u32,
    pub(crate) commit_seq: u64,
    pub(crate) record_type: u8,
    pub(crate) body_crc: u32,
}

/// Why a complete frame header is not valid.
pub(crate) enum FrameFault {
    /// The first four bytes are not the sync marker.
    Marker,
    /// The header checksum fails: a length, commit_seq or type was damaged.
    Checksum,
}

/// Decodes and checks a complete frame header.
pub(crate) fn decode_frame_header(frame: &[u8; FRAME_BYTES]) -> Result<FrameHeader, FrameFault> {
    let word =
        |at: usize| u32::from_le_bytes([frame[at], frame[at + 1], frame[at + 2], frame[at + 3]]);
    if word(0) != SYNC_MARKER {
        return Err(FrameFault::Marker);
    }
    if crc(&frame[..17]) != word(17) {
        return Err(FrameFault::Checksum);
    }
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&frame[8..16]);
    Ok(FrameHeader {
        length: word(4),
        commit_seq: u64::from_le_bytes(seq),
        record_type: frame[16],
        body_crc: word(21),
    })
}

/// Reads `source` to its end; `Some(bytes read)` when every byte is zero, `None` (and stops)
/// at the first non-zero byte.
fn zeros_to_end<R: Read>(source: &mut R) -> std::io::Result<Option<u64>> {
    let mut buf = [0u8; 8192];
    let mut total = 0u64;
    loop {
        let n = match source.read(&mut buf) {
            Ok(0) => return Ok(Some(total)),
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if buf[..n].iter().any(|&b| b != 0) {
            return Ok(None);
        }
        total += n as u64;
    }
}

fn put_kind(out: &mut Vec<u8>, kind: ElementKind) {
    put_u8(
        out,
        match kind {
            ElementKind::Vertex => 1,
            ElementKind::Edge => 2,
        },
    );
}

fn put_labels(out: &mut Vec<u8>, labels: &[String]) {
    put_uvar(out, labels.len() as u64);
    for label in labels {
        put_str(out, label);
    }
}

/// The commit payload of `changes` in its single-record form (format spec 9.3), whatever its
/// size (tests build records with it).
#[cfg(test)]
pub(crate) fn encode_commit(changes: &ChangeSet, codec: Codec) -> Result<Vec<u8>, PersistError> {
    Ok(encode_commit_body(changes, codec)?.single_payload())
}

/// The uncompressed body bytes from which the encoder checks the memory ceiling, doubling
/// each time (format spec 9.3.1: the body is built in memory once).
const ENCODE_CHECK_FROM: usize = 1 << 20;

/// Encodes the commit body of `changes` (format spec 9.3) once; the journal writes it as one
/// Commit record or as Commit parts ([`EncodedCommit::parts`]).
pub(crate) fn encode_commit_body(
    changes: &ChangeSet,
    codec: Codec,
) -> Result<EncodedCommit, PersistError> {
    let fail = |reason: String| {
        PersistError::encode(
            format!("the change set of commit {}", changes.commit_seq()),
            reason,
        )
    };
    let mut body = Vec::new();
    let mut next_check = ENCODE_CHECK_FROM;
    put_opt_str(&mut body, changes.metadata().principal());
    put_properties(
        &mut body,
        Some(changes.metadata().attributes()),
        &mut InlineKeys,
    )
    .map_err(fail)?;
    put_uvar(&mut body, changes.mutations().len() as u64);
    for mutation in changes.mutations() {
        if body.len() >= next_check {
            // The body grows by doubling: room for the next growth is checked against the
            // process memory ceiling (check point "commit encoding").
            let len = body.len() as u64;
            crate::traversal::memory_budget::ensure_room(COMMIT_ENCODING_OPERATION, len, len)?;
            next_check = body.len().saturating_mul(2);
        }
        match mutation {
            Mutation::AddVertex {
                id,
                labels,
                properties,
            } => {
                put_u8(&mut body, 1);
                put_str(&mut body, id);
                put_labels(&mut body, labels);
                put_properties(&mut body, Some(properties), &mut InlineKeys).map_err(fail)?;
            }
            Mutation::DropVertex { id, before } => {
                put_u8(&mut body, 2);
                put_str(&mut body, id);
                put_labels(&mut body, &before.labels);
                put_properties(&mut body, Some(&before.properties), &mut InlineKeys)
                    .map_err(fail)?;
            }
            Mutation::AddEdge {
                id,
                label,
                out_id,
                in_id,
                properties,
            } => {
                put_u8(&mut body, 3);
                put_str(&mut body, id);
                put_opt_str(&mut body, label.as_deref());
                put_str(&mut body, out_id);
                put_str(&mut body, in_id);
                put_properties(&mut body, Some(properties), &mut InlineKeys).map_err(fail)?;
            }
            Mutation::DropEdge { id, before } => {
                put_u8(&mut body, 4);
                put_str(&mut body, id);
                put_opt_str(&mut body, before.label.as_deref());
                put_str(&mut body, &before.out_id);
                put_str(&mut body, &before.in_id);
                put_properties(&mut body, Some(&before.properties), &mut InlineKeys)
                    .map_err(fail)?;
            }
            Mutation::SetProperty {
                element,
                key,
                before,
                after,
            } => {
                put_u8(&mut body, 5);
                put_kind(&mut body, element.kind);
                put_str(&mut body, &element.id);
                put_str(&mut body, key);
                match before {
                    None => put_u8(&mut body, tag::ABSENT),
                    Some(value) => put_value(&mut body, value, &mut InlineKeys).map_err(fail)?,
                }
                put_value(&mut body, after, &mut InlineKeys).map_err(fail)?;
            }
            Mutation::RemoveProperty {
                element,
                key,
                before,
            } => {
                put_u8(&mut body, 6);
                put_kind(&mut body, element.kind);
                put_str(&mut body, &element.id);
                put_str(&mut body, key);
                put_value(&mut body, before, &mut InlineKeys).map_err(fail)?;
            }
            Mutation::AddLabel { id, label } => {
                put_u8(&mut body, 7);
                put_str(&mut body, id);
                put_str(&mut body, label);
            }
            Mutation::RemoveLabel { id, label } => {
                put_u8(&mut body, 8);
                put_str(&mut body, id);
                put_str(&mut body, label);
            }
            Mutation::SetEdgeLabel { id, before, after } => {
                put_u8(&mut body, 11);
                put_str(&mut body, id);
                put_opt_str(&mut body, before.as_deref());
                put_str(&mut body, after);
            }
            Mutation::SetSchema { before, after } => {
                put_u8(&mut body, 9);
                put_str(&mut body, &before.to_json());
                put_str(&mut body, &after.to_json());
            }
            Mutation::SetDefinition {
                kind,
                name,
                before,
                after,
            } => {
                if before.is_none() && after.is_none() {
                    return Err(fail(format!(
                        "the change of the {kind} '{name}' has neither a before nor an after image"
                    )));
                }
                // Format spec 16 rule 3: a writer never stores a critical definition of a kind
                // it does not know (it could not keep it consistent with the data).
                if let Some(after) = after
                    && after.flags().is_critical()
                    && after.is_opaque()
                {
                    return Err(fail(format!(
                        "it stores the critical definition '{name}' of {kind}, which this \
                         version does not know and so cannot keep consistent"
                    )));
                }
                put_u8(&mut body, 10);
                put_u8(&mut body, kind.code());
                put_str(&mut body, name);
                for image in [before, after] {
                    match image {
                        None => put_u8(&mut body, 0),
                        Some(definition) => {
                            put_u8(&mut body, 1);
                            DefinitionRecord::encode(definition)
                                .map_err(fail)?
                                .put(&mut body);
                        }
                    }
                }
            }
        }
    }
    let mut stamp = Vec::with_capacity(STAMP_BYTES);
    put_u64(&mut stamp, changes.commit_seq());
    put_i64(&mut stamp, changes.committed_at());
    put_u64(&mut stamp, changes.node_sequence());
    put_u64(&mut stamp, changes.edge_sequence());
    Ok(EncodedCommit {
        commit_seq: changes.commit_seq(),
        stamp,
        body,
        codec,
    })
}

/// Size of the commit stamp: `commit_seq u64 | time i64 | node_sequence u64 | edge_sequence u64`.
const STAMP_BYTES: usize = 32;

/// What the memory ceiling names when encoding a commit body does not fit.
pub(crate) const COMMIT_ENCODING_OPERATION: &str = "commit encoding";

/// What the memory ceiling names when assembling a commit in parts does not fit.
pub(crate) const COMMIT_READING_OPERATION: &str = "reading a commit in parts";

/// A commit's encoded body (format spec 9.3), built once and written either as one Commit
/// record ([`Self::single_payload`]) or as Commit parts ([`Self::put_part`], 9.3.1).
pub(crate) struct EncodedCommit {
    commit_seq: u64,
    stamp: Vec<u8>,
    body: Vec<u8>,
    codec: Codec,
}

impl EncodedCommit {
    /// How many parts of at most `part_bytes` uncompressed body bytes the commit needs: `1`
    /// is the single-record form (format spec 9.3), more are Commit parts (9.3.1). Refuses
    /// more than [`MAX_COMMIT_PARTS`] parts with [`PersistError::CommitTooLarge`].
    pub(crate) fn parts(&self, part_bytes: usize) -> Result<u32, PersistError> {
        let part_bytes = part_bytes.clamp(1, MAX_BLOCK_BYTES);
        let parts = self.body.len().div_ceil(part_bytes).max(1);
        match u32::try_from(parts) {
            Ok(parts) if parts <= MAX_COMMIT_PARTS => Ok(parts),
            _ => Err(PersistError::CommitTooLarge {
                commit_seq: self.commit_seq,
                bytes: self.body.len(),
                limit: part_bytes.saturating_mul(MAX_COMMIT_PARTS as usize),
            }),
        }
    }

    /// The payload of the single Commit record (format spec 9.3). The body must be at most
    /// [`MAX_BLOCK_BYTES`] ([`Self::parts`] = 1 for a part size within that bound).
    pub(crate) fn single_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.body.len() / 2 + 40);
        out.extend_from_slice(&self.stamp);
        put_slice(&mut out, &self.body, self.codec);
        out
    }

    /// Writes the payload of part `part` of `parts` (format spec 9.3.1) into `out` (cleared
    /// first): `part u32 | parts u32`, the stamp and body length in part 0, then the
    /// part's slice of at most `part_bytes` body bytes, compressed on its own.
    pub(crate) fn put_part(&self, part: u32, parts: u32, part_bytes: usize, out: &mut Vec<u8>) {
        let part_bytes = part_bytes.clamp(1, MAX_BLOCK_BYTES);
        out.clear();
        put_u32(out, part);
        put_u32(out, parts);
        if part == 0 {
            out.extend_from_slice(&self.stamp);
            put_u64(out, self.body.len() as u64);
        }
        let from = (part as usize)
            .saturating_mul(part_bytes)
            .min(self.body.len());
        let to = from.saturating_add(part_bytes).min(self.body.len());
        put_slice(out, &self.body[from..to], self.codec);
    }
}

/// `codec u8 | data` (codec 0) or `codec u8 | uncompressed u32 | stored` (format spec 9.3,
/// 9.3.1); the slice is the rest of the payload either way.
fn put_slice(out: &mut Vec<u8>, raw: &[u8], codec: Codec) {
    let (codec_byte, compressed) = codec::compress(raw, codec);
    put_u8(out, codec_byte);
    match compressed {
        None => out.extend_from_slice(raw),
        Some(stored) => {
            // At most MAX_BLOCK_BYTES (1 GiB), so it fits a u32.
            put_u32(out, raw.len() as u32);
            out.extend_from_slice(&stored);
        }
    }
}

/// The largest number of parts of one commit (format spec 9.3.1): 64 TiB at the default part
/// size, i.e. no practical limit. Beyond it the journal vetoes the commit
/// ([`PersistError::CommitTooLarge`]).
pub(crate) const MAX_COMMIT_PARTS: u32 = 1 << 20;

/// The uncompressed body bytes per Commit part the journal writes (format spec 9.3.1; 64 MiB):
/// a commit body up to it stays one Commit record. A reader accepts any part size up to
/// [`MAX_BLOCK_BYTES`].
pub(crate) const PART_BODY_BYTES: usize = 64 << 20;

fn read_kind(r: &mut Reader<'_>) -> DecodeResult<ElementKind> {
    let at = r.offset();
    match r.u8()? {
        1 => Ok(ElementKind::Vertex),
        2 => Ok(ElementKind::Edge),
        other => Err(DecodeError::new(
            at,
            format!("unknown element kind {other}"),
        )),
    }
}

fn read_labels(r: &mut Reader<'_>) -> DecodeResult<Vec<String>> {
    let count = r.count(1)?;
    let mut labels = Vec::with_capacity(count);
    for _ in 0..count {
        labels.push(r.str()?.to_string());
    }
    Ok(labels)
}

/// Reads a schema image of op 9: text this version cannot parse is content of a newer version
/// ([`super::snapshot::parse_stored_schema`]), never damage.
fn read_schema(r: &mut Reader<'_>, newer: Option<&NewerContent>) -> DecodeResult<Box<GraphSchema>> {
    let at = r.offset();
    let text = r.str()?;
    super::snapshot::parse_stored_schema(text, at, newer).map(Box::new)
}

/// Reads `opt(definition)` of op 10: `u8 0` none, `u8 1` followed by a definition of the op's
/// kind and name (format spec 9.3).
fn read_definition_image(
    r: &mut Reader<'_>,
    kind: DefinitionKind,
    name: &str,
    max_depth: usize,
) -> DecodeResult<Option<Box<Definition>>> {
    let at = r.offset();
    match r.u8()? {
        0 => Ok(None),
        1 => {
            let record_at = r.offset();
            let record = DefinitionRecord::read(r)?;
            if record.kind != kind.code() || record.name != name {
                return Err(DecodeError::new(
                    record_at,
                    "a definition image of another kind or name than its change",
                ));
            }
            let definition = record.decode_in(max_depth, r.version()).map_err(|err| {
                DecodeError::new(
                    record_at,
                    format!(
                        "definition '{name}': {} (payload byte {})",
                        err.reason, err.offset
                    ),
                )
            })?;
            Ok(Some(Box::new(definition)))
        }
        other => Err(DecodeError::new(
            at,
            format!("invalid presence byte {other} of a definition image"),
        )),
    }
}

/// Decodes a commit payload into its [`ChangeSet`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decode_commit(payload: &[u8], max_depth: usize) -> DecodeResult<ChangeSet> {
    decode_commit_in(payload, max_depth, FORMAT_VERSION, None)
}

/// [`decode_commit`] of a record of a journal of file format `version` (its header's).
/// `newer`: a read-only load, which shows a schema this version cannot parse as no schema
/// ([`ReadOptions`](super::ReadOptions) of a read-only open).
pub(crate) fn decode_commit_in(
    payload: &[u8],
    max_depth: usize,
    version: u32,
    newer: Option<&NewerContent>,
) -> DecodeResult<ChangeSet> {
    let mut r = Reader::new(payload);
    let stamp = CommitStamp {
        commit_seq: r.u64()?,
        committed_at: r.i64()?,
        node_sequence: r.u64()?,
        edge_sequence: r.u64()?,
    };
    let codec_at = r.offset();
    let codec_byte = r.u8()?;
    let body_at = r.offset();
    let body = if codec_byte == 0 {
        std::borrow::Cow::Borrowed(r.bytes(r.remaining())?)
    } else {
        let uncompressed = r.u32()? as usize;
        let stored = r.bytes(r.remaining())?;
        codec::decompress(codec_byte, stored, uncompressed)
            .map_err(|reason| codec::decompress_error(codec_byte, codec_at, reason))?
    };
    let shift = |e: DecodeError| {
        let mut shifted =
            DecodeError::new(body_at, format!("{} (body byte {})", e.reason, e.offset));
        shifted.unsupported = e.unsupported;
        shifted
    };
    decode_body(&body, stamp, max_depth, version, newer).map_err(shift)
}

/// Assembles the body of a commit written in parts (format spec 9.3.1) from the payloads of its
/// Commit-part records in order. The body grows per part (never allocated from the declared
/// length: hostile input), each growth checked against the process memory ceiling first.
pub(crate) struct CommitParts {
    stamp: CommitStamp,
    parts: u32,
    next: u32,
    body_length: u64,
    body: Vec<u8>,
    operation: &'static str,
}

/// The `part u32 | parts u32` prefix of a Commit-part payload.
pub(crate) fn part_index(payload: &[u8]) -> DecodeResult<(u32, u32)> {
    let mut r = Reader::new(payload);
    Ok((r.u32()?, r.u32()?))
}

/// The outcome of adding a part: `Err` is damage (offsets within the payload), `Ok(Err(..))`
/// a refusal of the memory ceiling (never damage).
pub(crate) type PartResult<T> = DecodeResult<Result<T, PersistError>>;

impl CommitParts {
    /// Starts with the payload of part 0.
    pub(crate) fn start(payload: &[u8], operation: &'static str) -> PartResult<Self> {
        let mut r = Reader::new(payload);
        let part = r.u32()?;
        if part != 0 {
            return Err(DecodeError::new(
                0,
                format!("part {part} of a commit in parts without the parts before it"),
            ));
        }
        let parts_at = r.offset();
        let parts = r.u32()?;
        if !(2..=MAX_COMMIT_PARTS).contains(&parts) {
            return Err(DecodeError::new(
                parts_at,
                format!("a commit in {parts} parts (2 to {MAX_COMMIT_PARTS} are valid)"),
            ));
        }
        let stamp = CommitStamp {
            commit_seq: r.u64()?,
            committed_at: r.i64()?,
            node_sequence: r.u64()?,
            edge_sequence: r.u64()?,
        };
        let body_length = r.u64()?;
        let mut assembled = Self {
            stamp,
            parts,
            next: 0,
            body_length,
            body: Vec::new(),
            operation,
        };
        let slice_at = r.offset();
        let slice = r.bytes(r.remaining())?;
        if let Err(err) = assembled.add_slice(slice, slice_at)? {
            return Ok(Err(err));
        }
        Ok(Ok(assembled))
    }

    /// The commit sequence number of part 0.
    pub(crate) fn commit_seq(&self) -> u64 {
        self.stamp.commit_seq
    }

    /// The number of parts part 0 declares.
    pub(crate) fn parts(&self) -> u32 {
        self.parts
    }

    /// Whether every part was added.
    pub(crate) fn is_complete(&self) -> bool {
        self.next == self.parts
    }

    /// Adds the payload of the next part.
    pub(crate) fn push(&mut self, payload: &[u8]) -> PartResult<()> {
        let mut r = Reader::new(payload);
        let part = r.u32()?;
        let parts = r.u32()?;
        if parts != self.parts {
            return Err(DecodeError::new(
                4,
                format!(
                    "a part of a commit in {parts} parts where part 0 says {}",
                    self.parts
                ),
            ));
        }
        if part != self.next {
            return Err(DecodeError::new(
                0,
                format!("part {part} where part {} must follow", self.next),
            ));
        }
        let slice_at = r.offset();
        let slice = r.bytes(r.remaining())?;
        self.add_slice(slice, slice_at)
    }

    /// Decodes and appends one slice (`codec u8 | data` or `codec u8 | uncompressed u32 |
    /// stored`); `at` is the slice's offset in its payload.
    fn add_slice(&mut self, slice: &[u8], at: u64) -> PartResult<()> {
        let mut r = Reader::new(slice);
        let codec_byte = r.u8()?;
        let raw = if codec_byte == 0 {
            std::borrow::Cow::Borrowed(r.bytes(r.remaining())?)
        } else {
            let uncompressed = r.u32()? as usize;
            let stored = r.bytes(r.remaining())?;
            codec::decompress(codec_byte, stored, uncompressed)
                .map_err(|reason| codec::decompress_error(codec_byte, at, reason))?
        };
        if raw.is_empty() || raw.len() > MAX_BLOCK_BYTES {
            return Err(DecodeError::new(
                at,
                format!(
                    "a part of {} body bytes (1 to {MAX_BLOCK_BYTES} are valid)",
                    raw.len()
                ),
            ));
        }
        let total = self.body.len() as u64 + raw.len() as u64;
        if total > self.body_length {
            return Err(DecodeError::new(
                at,
                format!(
                    "the parts hold more than the {} body bytes part 0 declares",
                    self.body_length
                ),
            ));
        }
        if self.body.capacity() - self.body.len() < raw.len() {
            // A growth allocates up to twice the capacity: room for it is checked first.
            let grow = (self.body.capacity() as u64).saturating_mul(2).max(total);
            let resident = self.body.len() as u64;
            if let Err(err) =
                crate::traversal::memory_budget::ensure_room(self.operation, grow, resident)
            {
                return Ok(Err(err.into()));
            }
            self.body.reserve(raw.len());
        }
        self.body.extend_from_slice(&raw);
        self.next += 1;
        Ok(Ok(()))
    }

    /// Decodes the assembled commit; every part must have been added. `Err` offsets are body
    /// offsets (named "body byte N" in the reason).
    pub(crate) fn finish(
        self,
        max_depth: usize,
        version: u32,
        newer: Option<&NewerContent>,
    ) -> DecodeResult<ChangeSet> {
        if !self.is_complete() {
            return Err(DecodeError::new(
                0,
                format!("{} of {} parts", self.next, self.parts),
            ));
        }
        if self.body.len() as u64 != self.body_length {
            return Err(DecodeError::new(
                0,
                format!(
                    "the parts hold {} body bytes, part 0 declares {}",
                    self.body.len(),
                    self.body_length
                ),
            ));
        }
        decode_body(&self.body, self.stamp, max_depth, version, newer).map_err(|e| {
            let mut shifted = DecodeError::new(0, format!("{} (body byte {})", e.reason, e.offset));
            shifted.unsupported = e.unsupported;
            shifted
        })
    }
}

fn decode_body(
    body: &[u8],
    stamp: CommitStamp,
    max_depth: usize,
    version: u32,
    newer: Option<&NewerContent>,
) -> DecodeResult<ChangeSet> {
    let mut r = Reader::new(body).with_version(version);
    let keys = InlineKeys;
    let mut metadata = CommitMetadata::new();
    if let Some(principal) = r.opt_str()? {
        metadata = metadata.with_principal(principal);
    }
    for (key, value) in read_properties(&mut r, &keys, max_depth)? {
        metadata = metadata.with_attribute(&key, value);
    }
    let count = r.count(2)?;
    let mut mutations = Vec::with_capacity(count);
    for _ in 0..count {
        let at = r.offset();
        let mutation = match r.u8()? {
            1 => Mutation::AddVertex {
                id: r.str()?.to_string(),
                labels: read_labels(&mut r)?,
                properties: read_properties(&mut r, &keys, max_depth)?,
            },
            2 => Mutation::DropVertex {
                id: r.str()?.to_string(),
                before: VertexState {
                    labels: read_labels(&mut r)?,
                    properties: read_properties(&mut r, &keys, max_depth)?,
                },
            },
            3 => Mutation::AddEdge {
                id: r.str()?.to_string(),
                label: r.opt_str()?.map(str::to_string),
                out_id: r.str()?.to_string(),
                in_id: r.str()?.to_string(),
                properties: read_properties(&mut r, &keys, max_depth)?,
            },
            4 => Mutation::DropEdge {
                id: r.str()?.to_string(),
                before: EdgeState {
                    label: r.opt_str()?.map(str::to_string),
                    out_id: r.str()?.to_string(),
                    in_id: r.str()?.to_string(),
                    properties: read_properties(&mut r, &keys, max_depth)?,
                },
            },
            5 => {
                let kind = read_kind(&mut r)?;
                let id = r.str()?.to_string();
                let key = r.str()?.to_string();
                let before_tag = r.u8()?;
                let before = if before_tag == tag::ABSENT {
                    None
                } else {
                    Some(codec::read_tagged_value(
                        &mut r, before_tag, &keys, max_depth, 0,
                    )?)
                };
                Mutation::SetProperty {
                    element: ElementRef { kind, id },
                    key,
                    before,
                    after: read_value(&mut r, &keys, max_depth)?,
                }
            }
            6 => {
                let kind = read_kind(&mut r)?;
                Mutation::RemoveProperty {
                    element: ElementRef {
                        kind,
                        id: r.str()?.to_string(),
                    },
                    key: r.str()?.to_string(),
                    before: read_value(&mut r, &keys, max_depth)?,
                }
            }
            7 => Mutation::AddLabel {
                id: r.str()?.to_string(),
                label: r.str()?.to_string(),
            },
            8 => Mutation::RemoveLabel {
                id: r.str()?.to_string(),
                label: r.str()?.to_string(),
            },
            9 => Mutation::SetSchema {
                before: read_schema(&mut r, newer)?,
                after: read_schema(&mut r, newer)?,
            },
            10 => {
                let kind = DefinitionKind::from_code(r.u8()?);
                let name = r.str()?.to_string();
                let before = read_definition_image(&mut r, kind, &name, max_depth)?;
                let after = read_definition_image(&mut r, kind, &name, max_depth)?;
                if before.is_none() && after.is_none() {
                    return Err(DecodeError::new(
                        at,
                        "a definition change without a before and an after image",
                    ));
                }
                Mutation::SetDefinition {
                    kind,
                    name,
                    before,
                    after,
                }
            }
            11 => Mutation::SetEdgeLabel {
                id: r.str()?.to_string(),
                before: r.opt_str()?.map(str::to_string),
                after: r.str()?.to_string(),
            },
            other => return Err(DecodeError::new(at, format!("unknown mutation op {other}"))),
        };
        mutations.push(mutation);
    }
    if !r.is_empty() {
        return Err(r.error("trailing bytes after the last mutation"));
    }
    Ok(ChangeSet::new(stamp, metadata, mutations))
}

/// Encodes a mark payload (format spec 9.4).
pub(crate) fn encode_mark(mark: &Mark) -> Vec<u8> {
    let mut out = Vec::with_capacity(mark.name().len() + 20);
    put_u64(&mut out, mark.commit_seq());
    put_i64(&mut out, mark.time());
    put_str(&mut out, mark.name());
    out
}

fn decode_mark(payload: &[u8]) -> DecodeResult<Mark> {
    let mut r = Reader::new(payload);
    let commit_seq = r.u64()?;
    let time = r.i64()?;
    let name = r.str()?.to_string();
    if !r.is_empty() {
        return Err(r.error("trailing bytes after the mark"));
    }
    Ok(Mark::new(name, commit_seq, time))
}

/// One record of a journal.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum JournalRecord {
    /// A committed unit.
    Commit(ChangeSet),
    /// A mark.
    Mark(Mark),
    /// Informational: a snapshot at `commit_seq` exists (written by the Store).
    #[non_exhaustive]
    Checkpoint {
        /// The snapshot's position.
        commit_seq: u64,
        /// When the checkpoint was taken.
        time: i64,
    },
}

/// The incomplete record at the end of a journal: an interrupted last write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TornTail {
    /// The index of the journal (in a recovery's list; `0` for [`read_journal`]).
    pub journal: usize,
    /// Where the incomplete record starts.
    pub offset: u64,
    /// How many bytes of it are present (they are ignored).
    pub bytes: u64,
}

/// Reads the records of one journal stream in order ([`read_journal`]).
///
/// It is an iterator of records; after the last one, [`JournalReader::torn_tail`] says whether
/// the stream ended in an incomplete record (an interrupted write, ignored). A complete record
/// whose checksum fails, a wrong sync marker or an unknown record type is damage: the iterator
/// yields [`PersistError::Corrupt`] and stops.
pub struct JournalReader<R: Read> {
    source: R,
    header: WalHeader,
    offset: u64,
    index: usize,
    max_depth: usize,
    torn: Option<TornTail>,
    /// The file format version of the header (records are read with its value tags).
    version: u32,
    done: bool,
    /// A read-only load's lenient schema reading ([`decode_commit_in`]).
    newer: Option<NewerContent>,
    /// What a refusal of the memory ceiling names (a record body read whole):
    /// `"recovery"` unless the replay names another operation (`"store open"`, ...).
    operation: &'static str,
    /// `(record start, frame commit_seq)` of the last commit record whose body this version
    /// could not decode ([`PersistError::UnsupportedContent`]; the reader can go on after it).
    unsupported_seq: Option<(u64, u64)>,
}

/// What [`JournalReader`] found at a record boundary.
enum RawFrame {
    /// The end of the stream.
    End,
    /// An interrupted last write of `bytes` bytes (format spec 9.6).
    Torn { bytes: u64 },
    /// A record whose checksums hold.
    Record { header: FrameHeader, body: Vec<u8> },
}

/// Opens a journal stream: reads and checks its header ([`JournalReader`]).
pub fn read_journal<R: Read>(reader: R) -> Result<JournalReader<R>, PersistError> {
    JournalReader::open(reader, 0, crate::value::depth::DEFAULT_MAX_VALUE_DEPTH)
}

/// Reads until `buf` is full or the stream ends; returns how many bytes were read.
fn read_full<R: Read>(source: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match source.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

impl<R: Read> JournalReader<R> {
    pub(crate) fn open(
        mut source: R,
        index: usize,
        max_depth: usize,
    ) -> Result<Self, PersistError> {
        let location = format!("journal {index}");
        let mut head = [0u8; WAL_HEADER_BYTES];
        let got = read_full(&mut source, &mut head)
            .map_err(|err| PersistError::io("reading a journal", err))?;
        if got < WAL_HEADER_BYTES {
            return Err(PersistError::corrupt(
                location,
                got as u64,
                "journal header truncated",
            ));
        }
        let (header, version) =
            WalHeader::decode_versioned(&head).map_err(|err| err.at(location, 0))?;
        Ok(Self {
            source,
            header,
            offset: WAL_HEADER_BYTES as u64,
            index,
            max_depth,
            torn: None,
            version,
            done: false,
            newer: None,
            operation: super::recover::RECOVERY_OPERATION,
            unsupported_seq: None,
        })
    }

    /// The same reader showing a schema this version cannot parse as no schema, noted in
    /// `newer` (a read-only load only, format spec 16).
    pub(crate) fn with_newer_schema(mut self, newer: Option<NewerContent>) -> Self {
        self.newer = newer;
        self
    }

    /// The same reader naming `operation` when the memory ceiling refuses a record.
    pub(crate) fn with_operation(mut self, operation: &'static str) -> Self {
        self.operation = operation;
        self
    }

    /// [`Self::next_record`], going on past records whose checksums hold but whose body this
    /// version cannot decode (content of a newer version, format spec 16): for scans that
    /// only need what they can read (marks, counts). `skipped` gets the frame commit_seq of
    /// every skipped commit.
    pub(crate) fn next_known_record(
        &mut self,
        skipped: &mut impl FnMut(u64),
    ) -> Result<Option<(u64, u64, JournalRecord)>, PersistError> {
        loop {
            match self.next_record() {
                Err(PersistError::UnsupportedContent { .. }) => {
                    if let Some((_, seq)) = self.unsupported_seq {
                        skipped(seq);
                    }
                }
                other => return other,
            }
        }
    }

    /// `(record start, frame commit_seq)` of the last commit record [`Self::next_record`]
    /// refused with [`PersistError::UnsupportedContent`]: its checksums held, so the reader may
    /// go on after it.
    pub(crate) fn unsupported_seq(&self) -> Option<(u64, u64)> {
        self.unsupported_seq
    }

    /// The lineage id in the header.
    pub fn graph_id(&self) -> u128 {
        self.header.graph_id
    }

    /// The file format version of the journal's header (its records are read with it).
    pub(crate) fn format_version(&self) -> u32 {
        self.version
    }

    /// The first commit sequence number the journal may contain.
    pub fn first_commit_seq(&self) -> u64 {
        self.header.first_commit_seq
    }

    /// When the journal was started (microseconds since the Unix epoch, UTC).
    pub fn created_at(&self) -> i64 {
        self.header.created_at
    }

    /// The byte offset after the last record read.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    /// The incomplete record the stream ended in, once the iterator returned `None`.
    pub fn torn_tail(&self) -> Option<TornTail> {
        self.torn
    }

    fn corrupt(&mut self, offset: u64, reason: impl Into<String>) -> PersistError {
        self.done = true;
        PersistError::corrupt(format!("journal {}", self.index), offset, reason)
    }

    /// A commit record whose checksums hold but whose semantic content (a schema) this version
    /// cannot interpret (format spec 16): content of a newer version, never damage. The reader
    /// stays usable: the next record follows.
    fn unsupported(&mut self, start: u64, offset: u64, seq: u64, reason: String) -> PersistError {
        self.unsupported_seq = Some((start, seq));
        PersistError::unsupported_content(
            format!("journal {}", self.index),
            offset,
            format!("the record of commit {seq}: {reason}"),
        )
    }

    /// Reads the record frame at `start` (the current position of the source): the end of the
    /// stream, a torn tail of `bytes` bytes (format spec 9.6), or a record whose checksums
    /// hold. Damage is an error (and ends the reader).
    fn read_frame(&mut self, start: u64) -> Result<RawFrame, PersistError> {
        let mut frame = [0u8; FRAME_BYTES];
        let got = read_full(&mut self.source, &mut frame)
            .map_err(|err| PersistError::io("reading a journal", err))?;
        if got == 0 {
            return Ok(RawFrame::End);
        }
        if got < FRAME_BYTES {
            // An incomplete frame header: an interrupted write of the last record, when its
            // bytes are a prefix of the sync marker or zeros (space the file system allocated
            // but the write never filled). Anything else is damage.
            let marker = SYNC_MARKER.to_le_bytes();
            let prefix = got.min(4);
            let zeros = frame[..got].iter().all(|&b| b == 0);
            if frame[..prefix] != marker[..prefix] && !zeros {
                return Err(self.corrupt(start, "bytes at the end that are not a record"));
            }
            return Ok(RawFrame::Torn { bytes: got as u64 });
        }
        let header = match decode_frame_header(&frame) {
            Ok(header) => header,
            Err(fault) => {
                // Zeros from a record boundary to the end: an unwritten tail, not a record.
                if frame.iter().all(|&b| b == 0) {
                    let rest = zeros_to_end(&mut self.source)
                        .map_err(|err| PersistError::io("reading a journal", err))?;
                    if let Some(rest) = rest {
                        return Ok(RawFrame::Torn {
                            bytes: FRAME_BYTES as u64 + rest,
                        });
                    }
                }
                let reason = match fault {
                    FrameFault::Marker => "sync marker missing",
                    FrameFault::Checksum => {
                        "record header checksum mismatch (a damaged length, commit_seq or type)"
                    }
                };
                return Err(self.corrupt(start, reason));
            }
        };
        let (length, commit_seq) = (header.length, header.commit_seq);
        // The body is read whole (its header's checksum holds, so the length is the writer's):
        // room for it is checked first.
        if let Err(err) =
            crate::traversal::memory_budget::ensure_room(self.operation, u64::from(length), 0)
        {
            self.done = true;
            return Err(err.into());
        }
        let mut body = Vec::with_capacity((length as usize).min(1 << 20));
        (&mut self.source)
            .take(u64::from(length))
            .read_to_end(&mut body)
            .map_err(|err| PersistError::io("reading a journal", err))?;
        if body.len() < length as usize {
            // The header's checksum holds, so the length is right: the write was cut.
            return Ok(RawFrame::Torn {
                bytes: (FRAME_BYTES + body.len()) as u64,
            });
        }
        if crc(&body) != header.body_crc {
            return Err(self.corrupt(
                start,
                format!("record of commit {commit_seq}: checksum mismatch"),
            ));
        }
        Ok(RawFrame::Record { header, body })
    }

    /// Ends the reader at a torn tail starting at `offset` with `bytes` bytes.
    fn torn_at(&mut self, offset: u64, bytes: u64) {
        self.torn = Some(TornTail {
            journal: self.index,
            offset,
            bytes,
        });
        self.done = true;
    }

    /// Reads the remaining parts of a commit in parts whose part 0 (`payload`, frame
    /// `commit_seq`) starts at `start` (format spec 9.3.1) and decodes the assembled commit.
    /// Fewer parts before the end of the stream are a torn tail cut at part 0 (9.6).
    fn read_commit_parts(
        &mut self,
        start: u64,
        commit_seq: u64,
        payload: Vec<u8>,
    ) -> Result<Option<(u64, u64, JournalRecord)>, PersistError> {
        let payload_at = start + FRAME_BYTES as u64;
        let mut parts = match CommitParts::start(&payload, COMMIT_READING_OPERATION) {
            Ok(Ok(parts)) => parts,
            Ok(Err(refused)) => {
                self.done = true;
                return Err(refused);
            }
            Err(err) => return Err(self.corrupt(payload_at + err.offset, err.reason)),
        };
        drop(payload);
        if parts.commit_seq() != commit_seq {
            return Err(self.corrupt(start, "frame and payload commit_seq differ"));
        }
        while !parts.is_complete() {
            let at = self.offset;
            let (header, body) = match self.read_frame(at)? {
                RawFrame::End => {
                    self.torn_at(start, at - start);
                    return Ok(None);
                }
                RawFrame::Torn { bytes } => {
                    self.torn_at(start, at - start + bytes);
                    return Ok(None);
                }
                RawFrame::Record { header, body } => (header, body),
            };
            if header.record_type != RECORD_COMMIT_PART || header.commit_seq != commit_seq {
                return Err(self.corrupt(
                    at,
                    format!(
                        "a record of type {} for commit {} between the parts of commit \
                         {commit_seq} ({} parts)",
                        header.record_type,
                        header.commit_seq,
                        parts.parts()
                    ),
                ));
            }
            match parts.push(&body) {
                Ok(Ok(())) => {}
                Ok(Err(refused)) => {
                    self.done = true;
                    return Err(refused);
                }
                Err(err) => {
                    return Err(self.corrupt(at + FRAME_BYTES as u64 + err.offset, err.reason));
                }
            }
            self.offset = at + FRAME_BYTES as u64 + body.len() as u64;
        }
        let changes = parts
            .finish(self.max_depth, self.version, self.newer.as_ref())
            .map_err(|err| {
                let reason = format!("commit {commit_seq} in parts: {}", err.reason);
                if err.unsupported {
                    self.unsupported(start, start, commit_seq, reason)
                } else {
                    self.corrupt(start, reason)
                }
            })?;
        Ok(Some((start, commit_seq, JournalRecord::Commit(changes))))
    }

    /// The next record with its byte offset and frame commit_seq. A commit in parts (format
    /// spec 9.3.1) is one record at the offset of its part 0.
    pub(crate) fn next_record(
        &mut self,
    ) -> Result<Option<(u64, u64, JournalRecord)>, PersistError> {
        if self.done {
            return Ok(None);
        }
        let start = self.offset;
        let (header, body) = match self.read_frame(start)? {
            RawFrame::End => {
                self.done = true;
                return Ok(None);
            }
            RawFrame::Torn { bytes } => {
                self.torn_at(start, bytes);
                return Ok(None);
            }
            RawFrame::Record { header, body } => (header, body),
        };
        let (length, commit_seq) = (header.length, header.commit_seq);
        self.offset = start + FRAME_BYTES as u64 + u64::from(length);
        if header.record_type == RECORD_COMMIT_PART && self.version >= COMMIT_PARTS_FORMAT_VERSION {
            return self.read_commit_parts(start, commit_seq, body);
        }
        let payload = &body[..];
        let payload_at = start + FRAME_BYTES as u64;
        let record = match header.record_type {
            RECORD_COMMIT => {
                // Semantic content this version cannot interpret (a schema of a newer version)
                // behind valid checksums is never damage (format spec 16); a value encoding it
                // cannot decode is damage (value tags are guarded by the format version).
                let changes =
                    decode_commit_in(payload, self.max_depth, self.version, self.newer.as_ref())
                        .map_err(|err| {
                            if err.unsupported {
                                self.unsupported(
                                    start,
                                    payload_at + err.offset,
                                    commit_seq,
                                    err.reason,
                                )
                            } else {
                                self.corrupt(payload_at + err.offset, err.reason)
                            }
                        })?;
                if changes.commit_seq() != commit_seq {
                    return Err(self.corrupt(start, "frame and payload commit_seq differ"));
                }
                JournalRecord::Commit(changes)
            }
            RECORD_MARK => {
                let mark = decode_mark(payload)
                    .map_err(|err| self.corrupt(payload_at + err.offset, err.reason))?;
                if mark.commit_seq() != commit_seq {
                    return Err(self.corrupt(start, "frame and payload commit_seq differ"));
                }
                JournalRecord::Mark(mark)
            }
            RECORD_CHECKPOINT => {
                let mut r = Reader::new(payload);
                let (seq, time) = match (r.u64(), r.i64()) {
                    (Ok(seq), Ok(time)) if r.is_empty() => (seq, time),
                    _ => return Err(self.corrupt(payload_at, "malformed checkpoint record")),
                };
                JournalRecord::Checkpoint {
                    commit_seq: seq,
                    time,
                }
            }
            other => {
                return Err(self.corrupt(start + 16, format!("unknown record type {other}")));
            }
        };
        Ok(Some((start, commit_seq, record)))
    }
}

impl<R: Read> Iterator for JournalReader<R> {
    type Item = Result<JournalRecord, PersistError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_record() {
            Ok(Some((_, _, record))) => Some(Ok(record)),
            Ok(None) => None,
            Err(err) => {
                self.done = true;
                Some(Err(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(count: usize, label_len: usize) -> ChangeSet {
        let mutations = (0..count)
            .map(|i| Mutation::AddLabel {
                id: format!("v{i}"),
                label: format!("{i:0label_len$}"),
            })
            .collect();
        ChangeSet::new(
            CommitStamp {
                commit_seq: 5,
                committed_at: 1_700_000_000_000_000,
                node_sequence: 9,
                edge_sequence: 3,
            },
            CommitMetadata::new().with_principal("ann"),
            mutations,
        )
    }

    fn header() -> Vec<u8> {
        WalHeader {
            graph_id: 7,
            first_commit_seq: 5,
            created_at: 0,
        }
        .encode()
    }

    /// A journal stream (header, then the commit as written with `part_bytes`).
    fn stream(changes: &ChangeSet, codec: Codec, part_bytes: usize) -> (Vec<u8>, u32) {
        let mut out = header();
        let encoded = encode_commit_body(changes, codec).unwrap();
        let parts = encoded.parts(part_bytes).unwrap();
        if parts == 1 {
            let payload = encoded.single_payload();
            frame_record(&mut out, RECORD_COMMIT, changes.commit_seq(), &payload).unwrap();
        } else {
            let mut payload = Vec::new();
            for part in 0..parts {
                encoded.put_part(part, parts, part_bytes, &mut payload);
                frame_record(&mut out, RECORD_COMMIT_PART, changes.commit_seq(), &payload).unwrap();
            }
        }
        (out, parts)
    }

    type Read = (Vec<(u64, JournalRecord)>, Option<TornTail>);

    fn read_all(bytes: &[u8]) -> Result<Read, PersistError> {
        let mut reader = JournalReader::open(bytes, 0, 64)?;
        let mut records = Vec::new();
        while let Some((start, _, record)) = reader.next_record()? {
            records.push((start, record));
        }
        Ok((records, reader.torn_tail()))
    }

    fn damage(bytes: &[u8], what: &str) -> String {
        match read_all(bytes) {
            Err(PersistError::Corrupt { reason, .. }) => reason,
            other => panic!("{what}: {:?}", other.map(|r| r.0.len())),
        }
    }

    fn body_len(changes: &ChangeSet) -> usize {
        encode_commit_body(changes, Codec::None).unwrap().body.len()
    }

    #[test]
    fn a_commit_in_parts_round_trips_with_every_codec_and_part_size() {
        // Long, compressible labels: LZ4 compresses the slices of 4096 bytes and more.
        let original = changes(400, 40);
        for codec in [Codec::None, Codec::Lz4] {
            for part_bytes in [1, 7, 64, 100, 999, 4096, 5000, 1 << 20] {
                let (bytes, parts) = stream(&original, codec, part_bytes);
                let expected = body_len(&original).div_ceil(part_bytes).max(1) as u32;
                assert_eq!(parts, expected, "{part_bytes}");
                let (records, torn) = read_all(&bytes).unwrap();
                assert!(torn.is_none());
                assert_eq!(
                    records,
                    vec![(
                        WAL_HEADER_BYTES as u64,
                        JournalRecord::Commit(original.clone())
                    )],
                    "{codec:?} {part_bytes}"
                );
            }
        }
    }

    #[test]
    fn the_part_size_boundary_keeps_the_single_record_form() {
        let original = changes(10, 20);
        let len = body_len(&original);
        // Exactly the part size: one Commit record, byte-identical to the single form.
        let (exact, parts) = stream(&original, Codec::None, len);
        assert_eq!(parts, 1);
        assert_eq!(exact[WAL_HEADER_BYTES + 16], RECORD_COMMIT);
        let (big, _) = stream(&original, Codec::None, PART_BODY_BYTES);
        assert_eq!(exact, big);
        // One byte less: two parts, the second holding one byte.
        let (split, parts) = stream(&original, Codec::None, len - 1);
        assert_eq!(parts, 2);
        assert_eq!(split[WAL_HEADER_BYTES + 16], RECORD_COMMIT_PART);
        assert_eq!(read_all(&split).unwrap().0.len(), 1);
    }

    #[test]
    fn too_many_parts_are_refused_as_too_large() {
        let original = changes(2, 4);
        let encoded = encode_commit_body(&original, Codec::None).unwrap();
        let fake = EncodedCommit {
            body: vec![0; MAX_COMMIT_PARTS as usize + 1],
            ..encoded
        };
        assert_eq!(fake.parts(1 << 20).unwrap(), 2);
        match fake.parts(1) {
            Err(PersistError::CommitTooLarge {
                commit_seq, limit, ..
            }) => {
                assert_eq!(commit_seq, 5);
                assert_eq!(limit, MAX_COMMIT_PARTS as usize);
            }
            other => panic!("{:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn inconsistent_parts_are_damage() {
        let original = changes(20, 10);
        let encoded = encode_commit_body(&original, Codec::None).unwrap();
        let parts = encoded.parts(64).unwrap();
        assert!(parts >= 4);
        let payloads: Vec<Vec<u8>> = (0..parts)
            .map(|part| {
                let mut payload = Vec::new();
                encoded.put_part(part, parts, 64, &mut payload);
                payload
            })
            .collect();
        let build = |order: &[usize], edit: &dyn Fn(usize, &mut Vec<u8>)| {
            let mut out = header();
            for &index in order {
                let mut payload = payloads[index].clone();
                edit(index, &mut payload);
                frame_record(&mut out, RECORD_COMMIT_PART, 5, &payload).unwrap();
            }
            out
        };
        let all: Vec<usize> = (0..parts as usize).collect();
        // A missing middle part, swapped parts, a part without part 0.
        let mut missing = all.clone();
        missing.remove(1);
        damage(&build(&missing, &|_, _| {}), "missing");
        let mut swapped = all.clone();
        swapped.swap(1, 2);
        damage(&build(&swapped, &|_, _| {}), "swapped");
        damage(&build(&all[1..], &|_, _| {}), "no part 0");
        // `parts` differing between parts, a commit of one part.
        let reason = damage(
            &build(&all, &|i, p| {
                if i == 2 {
                    p[4] ^= 1;
                }
            }),
            "parts differ",
        );
        assert!(reason.contains("part 0 says"), "{reason}");
        let one = |p: &mut Vec<u8>| p[4..8].copy_from_slice(&1u32.to_le_bytes());
        damage(
            &build(&all, &|i, p| {
                if i == 0 {
                    one(p)
                }
            }),
            "one part",
        );
        // The body length differing from the sum of the slices.
        for flip in [1u8, 0x80] {
            let reason = damage(
                &build(&all, &|i, p| {
                    if i == 0 {
                        p[40] ^= flip;
                    }
                }),
                "body length",
            );
            assert!(reason.contains("body bytes"), "{reason}");
        }
        // Another commit's part or a record of another type between the parts.
        let mut other = header();
        frame_record(&mut other, RECORD_COMMIT_PART, 5, &payloads[0]).unwrap();
        frame_record(
            &mut other,
            RECORD_MARK,
            5,
            &encode_mark(&Mark::new("m", 5, 0)),
        )
        .unwrap();
        for part in &payloads[1..] {
            frame_record(&mut other, RECORD_COMMIT_PART, 5, part).unwrap();
        }
        let reason = damage(&other, "a mark between parts");
        assert!(reason.contains("between the parts"), "{reason}");
        let mut other = header();
        frame_record(&mut other, RECORD_COMMIT_PART, 5, &payloads[0]).unwrap();
        frame_record(&mut other, RECORD_COMMIT_PART, 6, &payloads[1]).unwrap();
        damage(&other, "a part of another commit");
    }

    #[test]
    fn a_commit_in_parts_cut_anywhere_is_a_torn_tail_at_part_zero() {
        let original = changes(20, 10);
        let (bytes, parts) = stream(&original, Codec::None, 50);
        assert!(parts > 3);
        for cut in WAL_HEADER_BYTES + 1..bytes.len() {
            let (records, torn) = read_all(&bytes[..cut]).unwrap();
            assert!(records.is_empty(), "{cut}");
            let torn = torn.unwrap_or_else(|| panic!("{cut}"));
            assert_eq!(torn.offset, WAL_HEADER_BYTES as u64, "{cut}");
            assert_eq!(torn.bytes, (cut - WAL_HEADER_BYTES) as u64, "{cut}");
        }
        // Zeros after a complete part 0 (space allocated, never written).
        let length = u32::from_le_bytes(bytes[68..72].try_into().unwrap()) as usize;
        let mut zeroed = bytes[..WAL_HEADER_BYTES + FRAME_BYTES + length].to_vec();
        let half = zeroed.len();
        zeroed.extend(std::iter::repeat_n(0, 300));
        let (_, torn) = read_all(&zeroed).unwrap();
        assert_eq!(torn.unwrap().bytes, (half + 300 - WAL_HEADER_BYTES) as u64);
    }

    #[test]
    fn every_flipped_bit_of_a_commit_in_parts_is_damage() {
        let original = changes(12, 10);
        let (bytes, parts) = stream(&original, Codec::None, 40);
        assert!(parts > 3);
        for at in WAL_HEADER_BYTES..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[at] ^= 1 << (at % 8);
            damage(&flipped, &format!("flip at {at}"));
        }
    }

    #[test]
    fn a_hostile_body_length_allocates_nothing_up_front() {
        let original = changes(20, 10);
        let encoded = encode_commit_body(&original, Codec::None).unwrap();
        let mut payload = Vec::new();
        encoded.put_part(0, 2, 64, &mut payload);
        payload[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        let parts = CommitParts::start(&payload, COMMIT_READING_OPERATION)
            .unwrap()
            .unwrap();
        assert!(parts.body.capacity() < 1 << 20);
    }

    #[test]
    fn the_memory_ceiling_refuses_assembling_a_commit_in_parts() {
        let original = changes(4000, 40);
        let (bytes, parts) = stream(&original, Codec::None, 4096);
        assert!(parts > 10);
        crate::traversal::memory_limit::memory_limit_scope(1, || match read_all(&bytes) {
            Err(PersistError::Graph(crate::error::GraphError::ResourceLimitExceeded {
                ..
            })) => {}
            other => panic!("{:?}", other.map(|r| r.0.len())),
        });
    }

    #[test]
    fn the_memory_ceiling_refuses_encoding_a_large_commit_body() {
        let original = changes(40_000, 40);
        crate::traversal::memory_limit::memory_limit_scope(1, || {
            match encode_commit_body(&original, Codec::None) {
                Err(PersistError::Graph(crate::error::GraphError::ResourceLimitExceeded {
                    operation,
                    ..
                })) => assert_eq!(operation, COMMIT_ENCODING_OPERATION),
                other => panic!("{:?}", other.map(|_| ())),
            }
        });
    }

    #[test]
    fn commit_parts_are_unknown_in_a_segment_of_an_older_version() {
        let original = changes(20, 10);
        let (mut bytes, _) = stream(&original, Codec::None, 64);
        let mut old = header();
        old[8..12].copy_from_slice(&(COMMIT_PARTS_FORMAT_VERSION - 1).to_le_bytes());
        let checksum = crc(&old[..60]);
        old[60..64].copy_from_slice(&checksum.to_le_bytes());
        bytes[..WAL_HEADER_BYTES].copy_from_slice(&old);
        let reason = damage(&bytes, "older version");
        assert!(reason.contains("unknown record type 4"), "{reason}");
    }
}
