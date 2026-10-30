//! Journal (write-ahead log) records over streams (format spec section 9): the
//! segment header, the record frame with its sync marker, the lossless commit payload of a
//! [`ChangeSet`], marks, and [`JournalReader`], which reads them back and tells a torn tail
//! (an incomplete last write) from damage.

use std::io::Read;

use super::codec::{
    self, Codec, DecodeError, DecodeResult, FORMAT_VERSION, InlineKeys, MAX_BLOCK_BYTES, Reader,
    crc, put_i64, put_opt_str, put_properties, put_str, put_u8, put_u32, put_u64, put_uvar,
    put_value, read_properties, read_value, tag,
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

/// Encodes the commit payload of `changes` (format spec 9.3).
pub(crate) fn encode_commit(changes: &ChangeSet, codec: Codec) -> Result<Vec<u8>, PersistError> {
    let fail = |reason: String| {
        PersistError::encode(
            format!("the change set of commit {}", changes.commit_seq()),
            reason,
        )
    };
    let mut body = Vec::new();
    put_opt_str(&mut body, changes.metadata().principal());
    put_properties(
        &mut body,
        Some(changes.metadata().attributes()),
        &mut InlineKeys,
    )
    .map_err(fail)?;
    put_uvar(&mut body, changes.mutations().len() as u64);
    for mutation in changes.mutations() {
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
    if body.len() > MAX_BLOCK_BYTES {
        // Format spec 9.3: one commit is one record; the journal's veto rolls the unit back.
        return Err(PersistError::CommitTooLarge {
            commit_seq: changes.commit_seq(),
            bytes: body.len(),
            limit: MAX_BLOCK_BYTES,
        });
    }
    let mut out = Vec::with_capacity(body.len() / 2 + 40);
    put_u64(&mut out, changes.commit_seq());
    put_i64(&mut out, changes.committed_at());
    put_u64(&mut out, changes.node_sequence());
    put_u64(&mut out, changes.edge_sequence());
    let (codec_byte, compressed) = codec::compress(&body, codec);
    put_u8(&mut out, codec_byte);
    match compressed {
        None => out.extend_from_slice(&body),
        Some(stored) => {
            put_u32(&mut out, body.len() as u32);
            out.extend_from_slice(&stored);
        }
    }
    Ok(out)
}

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

    /// The next record with its byte offset and frame commit_seq.
    pub(crate) fn next_record(
        &mut self,
    ) -> Result<Option<(u64, u64, JournalRecord)>, PersistError> {
        if self.done {
            return Ok(None);
        }
        let start = self.offset;
        let mut frame = [0u8; FRAME_BYTES];
        let got = read_full(&mut self.source, &mut frame)
            .map_err(|err| PersistError::io("reading a journal", err))?;
        if got == 0 {
            self.done = true;
            return Ok(None);
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
            self.torn = Some(TornTail {
                journal: self.index,
                offset: start,
                bytes: got as u64,
            });
            self.done = true;
            return Ok(None);
        }
        let header = match decode_frame_header(&frame) {
            Ok(header) => header,
            Err(fault) => {
                // Zeros from a record boundary to the end: an unwritten tail, not a record.
                if frame.iter().all(|&b| b == 0) {
                    let rest = zeros_to_end(&mut self.source)
                        .map_err(|err| PersistError::io("reading a journal", err))?;
                    if let Some(rest) = rest {
                        self.torn = Some(TornTail {
                            journal: self.index,
                            offset: start,
                            bytes: FRAME_BYTES as u64 + rest,
                        });
                        self.done = true;
                        return Ok(None);
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
        crate::traversal::memory_budget::ensure_room(self.operation, u64::from(length), 0)?;
        let mut body = Vec::with_capacity((length as usize).min(1 << 20));
        (&mut self.source)
            .take(u64::from(length))
            .read_to_end(&mut body)
            .map_err(|err| PersistError::io("reading a journal", err))?;
        if body.len() < length as usize {
            // The header's checksum holds, so the length is right: the write was cut.
            self.torn = Some(TornTail {
                journal: self.index,
                offset: start,
                bytes: (FRAME_BYTES + body.len()) as u64,
            });
            self.done = true;
            return Ok(None);
        }
        if crc(&body) != header.body_crc {
            return Err(self.corrupt(
                start,
                format!("record of commit {commit_seq}: checksum mismatch"),
            ));
        }
        self.offset = start + FRAME_BYTES as u64 + u64::from(length);
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
