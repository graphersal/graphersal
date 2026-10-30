//! The system journal of a Store (format spec 4.7, book page "System Journal"): the instance
//! file `INSTANCE`, the segments `journal/<20 digits>.jnl` (WAL-like frames: sync marker,
//! length, header and body CRC), their writer (batched syncs, rotation), reader (torn tail vs
//! damage, resync on the sync marker) and the copy helpers of backups.
//!
//! The journal decides nothing: nothing here ever fails an operation of the store. A problem
//! of the journal's files is a note (`verify`, a page's notes, a backup's warnings), never
//! store damage.

use std::io::Write;
use std::sync::Arc;

use super::files::{self, DirPath, EntryKind};
use crate::graph::CommitClock;
use crate::graph::system_journal::{
    InstanceId, JournalError, JournalPage, JournalQuery, JournalRecord, JournalWriter, Level,
    PersistedReader, SystemJournal, WriteFailure,
};
use crate::persist::codec::{
    DecodeError, FORMAT_VERSION, InlineKeys, Reader, crc, put_i64, put_properties, put_str, put_u8,
    put_u32, put_u64, read_properties,
};
use crate::persist::dir::StoreFile;
use crate::value::PropertyMap;

/// The journal's directory at the store root.
pub(crate) const JOURNAL_DIR: &str = "journal";
/// The extension of a journal segment.
pub(crate) const JOURNAL_EXTENSION: &str = "jnl";
/// Magic of a journal segment.
pub(crate) const JOURNAL_MAGIC: &[u8; 8] = b"GRSLJRNL";
/// Size of a journal segment header.
pub(crate) const JOURNAL_HEADER_BYTES: usize = 64;
/// The sync marker that starts every record frame (bytes `47 53 52 4A`, "GSRJ"; not the WAL's).
pub(crate) const JOURNAL_SYNC_MARKER: u32 = 0x4A52_5347;
/// Size of a record frame header: marker, length, seq, level, reserved, header CRC, body CRC.
pub(crate) const JOURNAL_FRAME_BYTES: usize = 26;
/// The largest record payload written or read.
pub(crate) const MAX_RECORD_PAYLOAD: usize = 1 << 20;
/// The largest segment read (a larger one is skipped with a note).
const MAX_SEGMENT_READ: u64 = 64 << 20;
/// The default segment size before a new one is started.
pub(crate) const DEFAULT_JOURNAL_SEGMENT_BYTES: u64 = 4 << 20;
/// The instance file at the store root (instance-local: never copied).
pub(crate) const INSTANCE_FILE: &str = "INSTANCE";
/// Magic of the instance file.
const INSTANCE_MAGIC: &[u8; 8] = b"GRSLINST";
/// The deepest value nesting a field may hold.
const MAX_FIELD_DEPTH: usize = 64;

/// The file name of the segment whose first seq is `first`.
pub(crate) fn segment_name(first: u64) -> String {
    format!("{first:020}.{JOURNAL_EXTENSION}")
}

/// The segments of the journal in `dir` (a store root), oldest first; none when there is no
/// `journal/` directory.
pub(crate) fn list_segments(dir: &DirPath) -> std::io::Result<Vec<(u64, DirPath)>> {
    let root = dir.join(JOURNAL_DIR);
    let entries = match root.list() {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let suffix = format!(".{JOURNAL_EXTENSION}");
    let mut out: Vec<(u64, DirPath)> = entries
        .into_iter()
        .filter(|(_, kind)| *kind == EntryKind::File)
        .filter_map(|(name, _)| {
            let seq = files::parse_seq(name.strip_suffix(&suffix)?)?;
            Some((seq, root.join(&name)))
        })
        .collect();
    out.sort_by_key(|(seq, _)| *seq);
    Ok(out)
}

/// Whether `name` (relative to the store root) is a journal segment (a ZIP entry).
#[cfg(feature = "persist-zip")]
pub(crate) fn is_segment_entry(name: &str) -> bool {
    name.strip_prefix(JOURNAL_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|file| file.strip_suffix(&format!(".{JOURNAL_EXTENSION}")))
        .is_some_and(|digits| files::parse_seq(digits).is_some())
}

// ---------------------------------------------------------------------------------------------
// Segment header, frames, payload (format spec 4.7)
// ---------------------------------------------------------------------------------------------

/// The header of a journal segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentHeader {
    pub(crate) version: u32,
    pub(crate) instance: u128,
    pub(crate) first_seq: u64,
    pub(crate) created_at: i64,
}

impl SegmentHeader {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(JOURNAL_HEADER_BYTES);
        out.extend_from_slice(JOURNAL_MAGIC);
        put_u32(&mut out, self.version);
        out.extend_from_slice(&self.instance.to_be_bytes());
        put_u64(&mut out, self.first_seq);
        put_i64(&mut out, self.created_at);
        out.resize(60, 0);
        let checksum = crc(&out);
        put_u32(&mut out, checksum);
        out
    }

    /// The header of `bytes` (at least 64 bytes); `Err` for a bad magic or checksum. A newer
    /// version decodes (the caller decides: the version walk refuses the store).
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < JOURNAL_HEADER_BYTES {
            return Err("the segment header is truncated".into());
        }
        if &bytes[..8] != JOURNAL_MAGIC {
            return Err("not a journal segment (bad magic)".into());
        }
        let stored = u32::from_le_bytes([bytes[60], bytes[61], bytes[62], bytes[63]]);
        if crc(&bytes[..60]) != stored {
            return Err("the segment header checksum does not match".into());
        }
        let mut r = Reader::new(&bytes[8..60]);
        let header = (|| -> Result<SegmentHeader, DecodeError> {
            Ok(SegmentHeader {
                version: r.u32()?,
                instance: r.u128_be()?,
                first_seq: r.u64()?,
                created_at: r.i64()?,
            })
        })()
        .map_err(|err| err.reason)?;
        Ok(header)
    }
}

/// Encodes one record as a frame (format spec 4.7): marker, length, seq, level, reserved, the
/// CRC-32C of these 18 bytes, the CRC-32C of the payload, the payload.
pub(crate) fn encode_frame(record: &JournalRecord) -> Result<Vec<u8>, String> {
    let payload = encode_payload(record)?;
    if payload.len() > MAX_RECORD_PAYLOAD {
        return Err(format!(
            "a journal record of {} bytes is larger than 1 MiB",
            payload.len()
        ));
    }
    let mut out = Vec::with_capacity(JOURNAL_FRAME_BYTES + payload.len());
    put_u32(&mut out, JOURNAL_SYNC_MARKER);
    put_u32(&mut out, payload.len() as u32);
    put_u64(&mut out, record.seq);
    put_u8(&mut out, record.level.code());
    put_u8(&mut out, 0);
    let header_crc = crc(&out);
    put_u32(&mut out, header_crc);
    put_u32(&mut out, crc(&payload));
    out.extend_from_slice(&payload);
    Ok(out)
}

fn encode_payload(record: &JournalRecord) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.extend_from_slice(&record.origin.id.to_be_bytes());
    put_u64(&mut out, record.origin_seq);
    put_i64(&mut out, record.time);
    put_str(&mut out, &record.source);
    put_str(&mut out, &record.kind);
    put_str(&mut out, &record.message);
    let mut presence = 0u8;
    if record.op.is_some() {
        presence |= 1;
    }
    if record.parent.is_some() {
        presence |= 2;
    }
    if record.commit.is_some() {
        presence |= 4;
    }
    if record.principal.is_some() {
        presence |= 8;
    }
    put_u8(&mut out, presence);
    for value in [record.op, record.parent, record.commit]
        .into_iter()
        .flatten()
    {
        put_u64(&mut out, value);
    }
    if let Some(principal) = &record.principal {
        put_str(&mut out, principal);
    }
    let fields: PropertyMap = record
        .fields
        .iter()
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect();
    put_properties(&mut out, Some(&fields), &mut InlineKeys)?;
    // The origin's readable name goes last (a reader that stops at the fields skips it).
    put_str(&mut out, &record.origin.name);
    Ok(out)
}

/// Decodes a payload whose CRC matched. `Err` when the envelope is unreadable (the record is
/// skipped with a note); unreadable fields of a newer version give a record with the single
/// field `fields_unreadable`.
fn decode_payload(seq: u64, level: u8, payload: &[u8]) -> Result<JournalRecord, String> {
    let mut r = Reader::new(payload);
    let envelope = (|| -> Result<_, DecodeError> {
        let origin = r.u128_be()?;
        let origin_seq = r.u64()?;
        let time = r.i64()?;
        let source = r.str()?.to_string();
        let kind = r.str()?.to_string();
        let message = r.str()?.to_string();
        let presence = r.u8()?;
        let op = if presence & 1 != 0 {
            Some(r.u64()?)
        } else {
            None
        };
        let parent = if presence & 2 != 0 {
            Some(r.u64()?)
        } else {
            None
        };
        let commit = if presence & 4 != 0 {
            Some(r.u64()?)
        } else {
            None
        };
        let principal = if presence & 8 != 0 {
            Some(r.str()?.to_string())
        } else {
            None
        };
        Ok((
            origin, origin_seq, time, source, kind, message, op, parent, commit, principal,
        ))
    })()
    .map_err(|err| format!("record {seq}: {}", err.reason))?;
    let (origin, origin_seq, time, source, kind, message, op, parent, commit, principal) = envelope;
    let (fields, name) = match read_properties(&mut r, &InlineKeys, MAX_FIELD_DEPTH) {
        Ok(map) => {
            let fields = map
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect();
            let name = r.str().map(str::to_string).unwrap_or_default();
            (fields, name)
        }
        Err(err) => (
            vec![(
                "fields_unreadable".to_string(),
                crate::value::ElementProperty::from(err.reason.as_str()),
            )],
            String::new(),
        ),
    };
    let mut record = JournalRecord {
        seq,
        origin: InstanceId::new(origin, name),
        origin_seq,
        time,
        level: Level::from_code(level),
        source,
        kind,
        message,
        op,
        parent,
        commit,
        principal,
        fields,
    };
    if record.origin.name.is_empty() {
        record.origin.name = "unknown".into();
    }
    Ok(record)
}

/// What reading one segment found.
#[derive(Debug, Default)]
pub(crate) struct SegmentScan {
    pub(crate) header: Option<SegmentHeader>,
    /// The records read (only those wanted, see [`scan_segment`]).
    pub(crate) records: Vec<JournalRecord>,
    /// How many valid frames there are.
    pub(crate) frames: u64,
    /// The highest seq of a valid frame.
    pub(crate) last_seq: Option<u64>,
    /// The time of the last valid record.
    pub(crate) last_time: Option<i64>,
    /// The end of the last valid complete frame (the header's end when there is none).
    pub(crate) valid_end: u64,
    /// The file ends inside a frame (an interrupted last write).
    pub(crate) torn_tail: bool,
    /// Damage met (skipped by resync).
    pub(crate) damage: Vec<String>,
}

/// Reads one segment's bytes: every valid frame, a torn tail, damage skipped by resyncing on
/// the sync marker. `keep` says which records (by seq) to decode and keep.
pub(crate) fn scan_segment(bytes: &[u8], keep: &mut dyn FnMut(u64) -> bool) -> SegmentScan {
    let mut scan = SegmentScan::default();
    match SegmentHeader::decode(bytes) {
        Ok(header) => scan.header = Some(header),
        Err(reason) => scan.damage.push(format!("header: {reason}")),
    }
    let start = JOURNAL_HEADER_BYTES.min(bytes.len());
    scan.valid_end = start as u64;
    let marker = JOURNAL_SYNC_MARKER.to_le_bytes();
    let mut at = start;
    // Set after damage: the frames before the next valid one are skipped.
    while at < bytes.len() {
        let rest = &bytes[at..];
        if rest.len() < JOURNAL_FRAME_BYTES {
            // A frame header cut off (or garbage shorter than one): a torn tail when it starts
            // like a frame or holds nothing but zeros.
            if rest.starts_with(&marker[..rest.len().min(4)]) || rest.iter().all(|b| *b == 0) {
                scan.torn_tail = true;
            } else {
                scan.damage.push(format!(
                    "byte {at}: {} unreadable byte(s) at the end",
                    rest.len()
                ));
            }
            break;
        }
        let header_ok = rest[..4] == marker
            && crc(&rest[..18]) == u32::from_le_bytes([rest[18], rest[19], rest[20], rest[21]]);
        if !header_ok {
            match resync(bytes, at + 1) {
                Some(next) => {
                    scan.damage
                        .push(format!("byte {at}: {} damaged byte(s) skipped", next - at));
                    at = next;
                    continue;
                }
                None => {
                    // No valid frame follows: zeros or an incomplete frame start are a torn
                    // tail, anything else damage at the end.
                    if rest[..4] == marker || rest.iter().all(|b| *b == 0) {
                        scan.torn_tail = true;
                    } else {
                        scan.damage.push(format!(
                            "byte {at}: {} unreadable byte(s) at the end",
                            rest.len()
                        ));
                    }
                    break;
                }
            }
        }
        let length = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let seq = u64::from_le_bytes(rest[8..16].try_into().unwrap_or([0; 8]));
        let level = rest[16];
        let body_crc = u32::from_le_bytes([rest[22], rest[23], rest[24], rest[25]]);
        if length > MAX_RECORD_PAYLOAD {
            scan.damage.push(format!(
                "byte {at}: a record of {length} bytes (larger than 1 MiB)"
            ));
            match resync(bytes, at + 1) {
                Some(next) => {
                    at = next;
                    continue;
                }
                None => break,
            }
        }
        if rest.len() < JOURNAL_FRAME_BYTES + length {
            // The payload runs past the end: an interrupted last write, unless a valid frame
            // follows inside it (then the length was written over: damage).
            match resync(bytes, at + 1) {
                Some(next) => {
                    scan.damage
                        .push(format!("byte {at}: a record cut short by the next one"));
                    at = next;
                    continue;
                }
                None => {
                    scan.torn_tail = true;
                    break;
                }
            }
        }
        let payload = &rest[JOURNAL_FRAME_BYTES..JOURNAL_FRAME_BYTES + length];
        let end = at + JOURNAL_FRAME_BYTES + length;
        if crc(payload) != body_crc {
            scan.damage.push(format!(
                "byte {at}: record {seq}: the body checksum does not match"
            ));
            at = end;
            continue;
        }
        scan.frames += 1;
        scan.last_seq = Some(scan.last_seq.map_or(seq, |last| last.max(seq)));
        scan.valid_end = end as u64;
        // The time sits at a fixed offset of the payload (after origin and origin_seq).
        if payload.len() >= 32 {
            let time = i64::from_le_bytes(payload[24..32].try_into().unwrap_or([0; 8]));
            scan.last_time = Some(scan.last_time.map_or(time, |last| last.max(time)));
        }
        if keep(seq) {
            match decode_payload(seq, level, payload) {
                Ok(record) => scan.records.push(record),
                Err(reason) => scan.damage.push(format!("byte {at}: {reason}")),
            }
        }
        at = end;
    }
    scan
}

/// The offset of the next valid frame header at or after `from` (sync marker plus a matching
/// header CRC).
fn resync(bytes: &[u8], from: usize) -> Option<usize> {
    let marker = JOURNAL_SYNC_MARKER.to_le_bytes();
    let mut at = from;
    while at + JOURNAL_FRAME_BYTES <= bytes.len() {
        let window = &bytes[at..];
        let found = window
            .windows(4)
            .position(|w| w == marker)
            .map(|offset| at + offset)?;
        if found + JOURNAL_FRAME_BYTES <= bytes.len() {
            let frame = &bytes[found..found + JOURNAL_FRAME_BYTES];
            if crc(&frame[..18]) == u32::from_le_bytes([frame[18], frame[19], frame[20], frame[21]])
            {
                return Some(found);
            }
        }
        at = found + 1;
    }
    None
}

/// What the memory check of a whole journal segment read names.
const JOURNAL_READ: &str = "system journal read";

/// A whole segment: refused when larger than [`MAX_SEGMENT_READ`], and checked against the
/// process memory ceiling before it is read (an `OutOfMemory` I/O error carrying the refusal).
fn read_segment(path: &DirPath) -> std::io::Result<Vec<u8>> {
    let len = path.len()?;
    if len > MAX_SEGMENT_READ {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a journal segment of {len} bytes (larger than {MAX_SEGMENT_READ})"),
        ));
    }
    crate::traversal::memory_budget::ensure_room_io(JOURNAL_READ, len)?;
    files::read_bounded(path, MAX_SEGMENT_READ)
}

// ---------------------------------------------------------------------------------------------
// The instance file
// ---------------------------------------------------------------------------------------------

/// Encodes `INSTANCE`: magic, version 1, the id, reserved, CRC of 0..56, the name, its CRC.
fn encode_instance(instance: &InstanceId) -> Vec<u8> {
    let mut out = Vec::with_capacity(80 + instance.name.len());
    out.extend_from_slice(INSTANCE_MAGIC);
    put_u32(&mut out, 1);
    out.extend_from_slice(&instance.id.to_be_bytes());
    out.resize(56, 0);
    let checksum = crc(&out);
    put_u32(&mut out, checksum);
    let start = out.len();
    put_str(&mut out, &instance.name);
    let name_crc = crc(&out[start..]);
    put_u32(&mut out, name_crc);
    out
}

fn decode_instance(bytes: &[u8]) -> Option<InstanceId> {
    if bytes.len() < 64 || &bytes[..8] != INSTANCE_MAGIC {
        return None;
    }
    if crc(&bytes[..56]) != u32::from_le_bytes([bytes[56], bytes[57], bytes[58], bytes[59]]) {
        return None;
    }
    let id = u128::from_be_bytes(bytes[12..28].try_into().ok()?);
    let rest = &bytes[60..];
    let mut r = Reader::new(rest);
    let name = r.str().ok()?.to_string();
    let used = r.offset() as usize;
    let stored = rest.get(used..used + 4)?;
    if crc(&rest[..used]) != u32::from_le_bytes(stored.try_into().ok()?) {
        return None;
    }
    Some(InstanceId::new(id, name))
}

/// The instance of the store in `dir`: read from `INSTANCE`, else new (and written when
/// `writable`; a failed write is logged: the journal decides nothing). A configured `name`
/// that differs from the stored one renames the instance (same id).
pub(crate) fn instance_of(dir: &DirPath, name: Option<&str>, writable: bool) -> InstanceId {
    let path = dir.join(INSTANCE_FILE);
    let stored = files::read_bounded(&path, 64 << 10)
        .ok()
        .and_then(|bytes| decode_instance(&bytes));
    let instance = match (stored, name) {
        (Some(stored), Some(name)) if !name.is_empty() && stored.name != name => {
            InstanceId::new(stored.id, name)
        }
        (Some(stored), _) => return stored,
        (None, name) => InstanceId::generate(name),
    };
    if writable
        && let Err(err) = files::write_atomic(dir, INSTANCE_FILE, &encode_instance(&instance))
    {
        tracing::warn!(%err, "the instance file of the store was not written");
    }
    instance
}

// ---------------------------------------------------------------------------------------------
// Writer and reader
// ---------------------------------------------------------------------------------------------

/// Appends records to the newest segment of a store's journal. Records below `error` are
/// kept in memory and written in batches (at the next `error` record, a checkpoint, the system
/// job `stats_flush`, an explicit flush and at close), so the journal never writes in the middle
/// of another operation of the store; an `error` record is written and synced at once.
struct SegmentWriter {
    root: DirPath,
    instance: u128,
    clock: CommitClock,
    max_bytes: u64,
    file: Option<Box<dyn StoreFile>>,
    /// The newest segment to continue in (opened at the first write): path and valid end.
    resume: Option<(DirPath, u64)>,
    len: u64,
    /// Frames not written yet, with their seqs.
    pending: Vec<(u64, Vec<u8>)>,
    pending_bytes: usize,
}

/// The pending bytes that force a write.
const MAX_PENDING_BYTES: usize = 1 << 20;

impl SegmentWriter {
    /// Starts a new segment whose first record is `seq`.
    fn rotate(&mut self, seq: u64) -> std::io::Result<()> {
        if let Some(mut file) = self.file.take() {
            let _ = file.sync();
        }
        let journal = self.root.join(JOURNAL_DIR);
        journal.create_dir_all()?;
        let path = journal.join(segment_name(seq));
        let mut file = path.create(false)?;
        let header = SegmentHeader {
            version: FORMAT_VERSION,
            instance: self.instance,
            first_seq: seq,
            created_at: self.clock.floor(),
        };
        file.write_all(&header.encode())?;
        file.sync()?;
        journal.store_dir().sync_dir(journal.name())?;
        self.len = JOURNAL_HEADER_BYTES as u64;
        self.file = Some(file);
        Ok(())
    }

    /// Writes and syncs every pending frame; on a failure the frames not written count as lost
    /// and the next write starts a new segment.
    fn write_out(&mut self) -> Result<(), WriteFailure> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let frames = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        let total = frames.len() as u64;
        let mut written = 0u64;
        let result = (|| -> std::io::Result<()> {
            if self.file.is_none()
                && let Some((path, end)) = self.resume.take()
                && end < self.max_bytes
            {
                self.file = Some(path.open_write(end)?);
                self.len = end;
            }
            for (seq, frame) in &frames {
                if self.file.is_none()
                    || (self.len > JOURNAL_HEADER_BYTES as u64
                        && self.len + frame.len() as u64 > self.max_bytes)
                {
                    self.rotate(*seq)?;
                }
                let file = self
                    .file
                    .as_mut()
                    .ok_or_else(|| std::io::Error::other("no journal segment open"))?;
                file.write_all(frame)?;
                self.len += frame.len() as u64;
                written += 1;
            }
            if let Some(file) = self.file.as_mut() {
                file.sync()?;
            }
            Ok(())
        })();
        result.map_err(|err| {
            self.file = None;
            self.resume = None;
            WriteFailure {
                reason: err.to_string(),
                // Written but not synced: kept by the OS in most cases, still counted.
                lost: (total - written).max(1),
            }
        })
    }
}

impl JournalWriter for SegmentWriter {
    fn append(&mut self, record: &JournalRecord, urgent: bool) -> Result<(), WriteFailure> {
        let frame = encode_frame(record).map_err(|reason| WriteFailure { reason, lost: 1 })?;
        self.pending_bytes += frame.len();
        self.pending.push((record.seq, frame));
        if urgent || self.pending_bytes >= MAX_PENDING_BYTES {
            return self.write_out();
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), WriteFailure> {
        self.write_out()
    }
}

/// Reads a store's journal segments (no lock; a store open elsewhere is read as it is).
pub(crate) struct StoredReader {
    pub(crate) root: DirPath,
}

impl PersistedReader for StoredReader {
    fn read(
        &self,
        after: u64,
        before: u64,
        query: &JournalQuery,
        limit: usize,
    ) -> Result<(Vec<JournalRecord>, Vec<String>), JournalError> {
        let segments =
            list_segments(&self.root).map_err(|err| JournalError::io("list its segments", err))?;
        let start = segments
            .iter()
            .rposition(|(first, _)| *first <= after.saturating_add(1))
            .unwrap_or(0);
        let mut records = Vec::new();
        let mut notes = Vec::new();
        for (index, (first, path)) in segments.iter().enumerate().skip(start) {
            if *first >= before {
                break;
            }
            let next_first = segments.get(index + 1).map(|(next, _)| *next);
            if next_first.is_some_and(|next| next <= after.saturating_add(1)) {
                continue;
            }
            let bytes = match read_segment(path) {
                Ok(bytes) => bytes,
                // The memory ceiling refuses the read: the query fails (no partial answer).
                Err(err) if err.kind() == std::io::ErrorKind::OutOfMemory => {
                    return Err(JournalError::MemoryLimit {
                        reason: format!("{}: {err}", path.relative_to(&self.root)),
                    });
                }
                Err(err) => {
                    notes.push(format!("{}: {err}", path.relative_to(&self.root)));
                    continue;
                }
            };
            let scan = scan_segment(&bytes, &mut |seq| seq > after && seq < before);
            for damage in &scan.damage {
                notes.push(format!("{}: {damage}", path.relative_to(&self.root)));
            }
            let mut found: Vec<JournalRecord> = scan
                .records
                .into_iter()
                .filter(|record| query.matches(record))
                .collect();
            found.sort_by_key(|record| record.seq);
            for record in found {
                if records.len() == limit {
                    return Ok((records, notes));
                }
                records.push(record);
            }
        }
        Ok((records, notes))
    }
}

/// How a store's journal is opened.
pub(crate) struct OpenJournal<'a> {
    pub(crate) dir: &'a DirPath,
    pub(crate) clock: &'a CommitClock,
    /// Whether records are written to the files (`false`: read-only opens, an overlay in memory).
    pub(crate) writable: bool,
    pub(crate) instance_name: Option<&'a str>,
    pub(crate) segment_bytes: u64,
}

/// Opens the system journal of the store in `options.dir`: the instance, the newest segment's
/// records (in memory, raising the clock to the tail time), the next seq, a torn tail cut
/// (writable), the writer. Never fails: an unreadable journal starts a new segment, logged.
pub(crate) fn open_journal(options: OpenJournal<'_>) -> Arc<SystemJournal> {
    let dir = options.dir;
    let instance = instance_of(dir, options.instance_name, options.writable);
    let segments = match list_segments(dir) {
        Ok(segments) => segments,
        Err(err) => {
            tracing::warn!(%err, store = %dir.display(), "the system journal's segments are not readable");
            Vec::new()
        }
    };
    let mut next_seq = 1u64;
    let mut primed = Vec::new();
    let mut continue_in: Option<(DirPath, u64)> = None;
    if let Some((first, path)) = segments.last() {
        next_seq = next_seq.max(*first);
        match read_segment(path) {
            Ok(bytes) => {
                let scan = scan_segment(&bytes, &mut |_| true);
                next_seq = next_seq
                    .max(scan.last_seq.map_or(0, |seq| seq + 1))
                    .max(first.saturating_add(scan.frames));
                options.clock.raise(scan.last_time.unwrap_or(0));
                let mut records = scan.records;
                records.sort_by_key(|record| record.seq);
                primed = records;
                let header_ok = scan.header.is_some_and(|header| {
                    header.instance == instance.id && header.version == FORMAT_VERSION
                });
                let clean = scan.damage.is_empty();
                if options.writable && scan.torn_tail && clean {
                    // An interrupted last write: cut by the next writer.
                    if let Err(err) = path.truncate(scan.valid_end) {
                        tracing::warn!(%err, "the torn tail of the system journal was not cut");
                    }
                }
                if header_ok && clean && (!scan.torn_tail || options.writable) {
                    continue_in = Some((path.clone(), scan.valid_end));
                }
            }
            Err(err) => {
                tracing::warn!(%err, "the newest system journal segment is not readable");
                // Never reuse a seq: past the most frames the segment can hold.
                let frames = path.len().unwrap_or(0) / JOURNAL_FRAME_BYTES as u64;
                next_seq = next_seq.max(first.saturating_add(frames).saturating_add(1));
            }
        }
    }
    let writer: Option<Box<dyn JournalWriter>> = options.writable.then(|| {
        Box::new(SegmentWriter {
            root: dir.clone(),
            instance: instance.id,
            clock: options.clock.clone(),
            max_bytes: options.segment_bytes.max(JOURNAL_HEADER_BYTES as u64 + 1),
            file: None,
            resume: continue_in,
            len: 0,
            pending: Vec::new(),
            pending_bytes: 0,
        }) as Box<dyn JournalWriter>
    });
    let reader: Arc<dyn PersistedReader> = Arc::new(StoredReader { root: dir.clone() });
    SystemJournal::build(
        instance,
        options.clock.clone(),
        next_seq,
        primed,
        writer,
        Some(reader),
    )
}

/// A page of the journal of a CLOSED store (or one open elsewhere): no lock, nothing written.
pub(crate) fn query_dir(dir: &DirPath, query: &JournalQuery) -> Result<JournalPage, JournalError> {
    let limit = query.page_size();
    let reader = StoredReader { root: dir.clone() };
    let (mut records, notes) = reader.read(query.after, u64::MAX, query, limit + 1)?;
    let mut page = JournalPage {
        notes,
        ..JournalPage::default()
    };
    if records.len() > limit {
        records.truncate(limit);
        page.more = true;
    }
    page.next_after = match (page.more, records.last()) {
        (true, Some(last)) => last.seq,
        _ => last_seq(dir).unwrap_or(0).max(query.after),
    };
    page.records = records;
    Ok(page)
}

/// The last seq of the stored journal (its newest segment).
fn last_seq(dir: &DirPath) -> Option<u64> {
    let segments = list_segments(dir).ok()?;
    let (first, path) = segments.last()?;
    let bytes = read_segment(path).ok()?;
    let scan = scan_segment(&bytes, &mut |_| false);
    Some(scan.last_seq.unwrap_or(first.saturating_sub(1)))
}

/// Writes one record into the journal of the store in `dir` that is NOT open (a new store:
/// `store.created`; an upgrade: `store.upgraded`), as a new segment. Best effort (logged).
pub(crate) fn append_closed(
    dir: &DirPath,
    instance_name: Option<&str>,
    kind: &'static crate::graph::system_journal::catalogue::EventKind,
    event: crate::graph::system_journal::Event,
) {
    let clock = CommitClock::new();
    let journal = open_journal(OpenJournal {
        dir,
        clock: &clock,
        writable: true,
        instance_name,
        segment_bytes: DEFAULT_JOURNAL_SEGMENT_BYTES,
    });
    journal.emit(kind, event);
    journal.detach_writer();
}

/// Writes `store.created`, the first record of a new store in `dir` (`how`: create, packed,
/// fork, attic_fork, repair; `from`: the source store's id and the commit it was made from).
pub(crate) fn record_created(
    dir: &DirPath,
    how: &str,
    from: Option<(u128, u64)>,
    instance_name: Option<&str>,
) {
    use crate::graph::system_journal::{Event, catalogue};
    let mut event = Event::new(match from {
        Some((id, commit)) => format!(
            "the store was created ({how}) from store {} at commit {commit}",
            crate::value::uuid::format_canonical(id)
        ),
        None => format!("the store was created ({how})"),
    })
    .field("how", how);
    if let Some((id, commit)) = from {
        event = event
            .field(
                "from_store_id",
                crate::value::uuid::format_canonical(id).to_string(),
            )
            .count("from_commit", commit);
    }
    append_closed(dir, instance_name, &catalogue::CREATED, event);
}

// ---------------------------------------------------------------------------------------------
// Copies (format spec 12.6 to 12.8) and verify notes
// ---------------------------------------------------------------------------------------------

/// One journal segment a backup copies: its relative name, source, how many bytes (a sealed
/// segment whole, the newest up to its last complete record) and how many valid frames.
pub(crate) struct CopiedSegment {
    pub(crate) relative: String,
    pub(crate) source: DirPath,
    pub(crate) len: u64,
    pub(crate) frames: u64,
}

/// The segments a full backup of `dir` copies, plus warnings about damage met (never stops the
/// backup).
pub(crate) fn plan_copy(dir: &DirPath) -> (Vec<CopiedSegment>, Vec<String>) {
    let mut warnings = Vec::new();
    let segments = match list_segments(dir) {
        Ok(segments) => segments,
        Err(err) => {
            warnings.push(format!("{JOURNAL_DIR}: not readable ({err}); not copied"));
            return (Vec::new(), warnings);
        }
    };
    let count = segments.len();
    let mut out = Vec::new();
    for (index, (first, path)) in segments.into_iter().enumerate() {
        let relative = format!("{JOURNAL_DIR}/{}", segment_name(first));
        let bytes = match read_segment(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                warnings.push(format!("{relative}: not readable ({err}); not copied"));
                continue;
            }
        };
        let scan = scan_segment(&bytes, &mut |_| false);
        for damage in &scan.damage {
            warnings.push(format!(
                "{relative}: {damage} (system journal; copied as it is)"
            ));
        }
        let newest = index + 1 == count;
        let len = if newest {
            scan.valid_end
        } else {
            bytes.len() as u64
        };
        out.push(CopiedSegment {
            relative,
            source: path,
            len,
            frames: scan.frames,
        });
    }
    (out, warnings)
}

/// Checks the copied segments where they landed (frame by frame): warnings, never damage.
pub(crate) fn check_copies(target: &DirPath, copied: &[CopiedSegment]) -> Vec<String> {
    let mut warnings = Vec::new();
    for segment in copied {
        let path = target.join(&segment.relative);
        match read_segment(&path) {
            Ok(bytes) => {
                let scan = scan_segment(&bytes, &mut |_| false);
                if bytes.len() as u64 != segment.len || scan.frames != segment.frames {
                    warnings.push(format!(
                        "{}: the copy holds {} record(s) in {} bytes, the store's {} in {} \
                         (system journal)",
                        segment.relative,
                        scan.frames,
                        bytes.len(),
                        segment.frames,
                        segment.len
                    ));
                }
            }
            Err(err) => warnings.push(format!(
                "{}: the copy is not readable ({err}) (system journal)",
                segment.relative
            )),
        }
    }
    warnings
}

/// Checks a segment read back from a ZIP archive.
#[cfg(feature = "persist-zip")]
pub(crate) fn check_copy_bytes(
    relative: &str,
    bytes: &[u8],
    copied: &[CopiedSegment],
) -> Vec<String> {
    let Some(segment) = copied.iter().find(|segment| segment.relative == relative) else {
        return vec!["a journal segment the backup did not copy".into()];
    };
    let scan = scan_segment(bytes, &mut |_| false);
    if bytes.len() as u64 != segment.len || scan.frames != segment.frames {
        return vec![format!(
            "the copy holds {} record(s) in {} bytes, the store's {} in {}",
            scan.frames,
            bytes.len(),
            segment.frames,
            segment.len
        )];
    }
    Vec::new()
}

/// Copies `copied` into the backup in `target` (a full backup; parents created).
pub(crate) fn write_copies(
    target: &DirPath,
    copied: &[CopiedSegment],
    report: &mut Vec<(String, u64)>,
) -> std::io::Result<u64> {
    let mut bytes = 0;
    if copied.is_empty() {
        return Ok(0);
    }
    target.join(JOURNAL_DIR).create_dir_all()?;
    for segment in copied {
        let data = read_segment(&segment.source)?;
        let take = (segment.len as usize).min(data.len());
        let mut out = target.join(&segment.relative).create(false)?;
        out.write_all(&data[..take])?;
        out.sync()?;
        report.push((segment.relative.clone(), take as u64));
        bytes += take as u64;
    }
    let journal = target.join(JOURNAL_DIR);
    journal.store_dir().sync_dir(journal.name())?;
    Ok(bytes)
}

/// The lengths of the journal segments in `target` (a backup), to put back with
/// [`restore_lengths`] when an increment is rolled back.
pub(crate) fn segment_lengths(target: &DirPath) -> Vec<(DirPath, u64)> {
    list_segments(target)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, path)| Some((path.clone(), path.len().ok()?)))
        .collect()
}

/// Puts the journal of `target` back to `before`: segments cut to their old lengths, segments
/// that were not there removed (best effort: the journal decides nothing).
pub(crate) fn restore_lengths(target: &DirPath, before: &[(DirPath, u64)]) {
    for (_, path) in list_segments(target).unwrap_or_default() {
        let result = match before.iter().find(|(old, _)| old.name() == path.name()) {
            Some((_, len)) if path.len().is_ok_and(|now| now > *len) => path.truncate(*len),
            Some(_) => Ok(()),
            None => path.remove_file(),
        };
        if let Err(err) = result {
            tracing::warn!(%err, "a journal segment of the backup was not rolled back");
        }
    }
}

/// Brings the journal of the backup in `target` up to the store's in `dir` (an increment,
/// format spec 12.7): segments the backup lacks are copied, the backup's newest segment gets
/// the source's tail appended when it is a prefix of the source's; anything else is left as it
/// is with a warning (the journal decides nothing), as is a source segment that cannot be
/// read. A failed WRITE into the backup fails it (the backup could not be written). Returns
/// the warnings and the files written.
pub(crate) fn sync_into_backup(
    dir: &DirPath,
    target: &DirPath,
) -> Result<(Vec<String>, Vec<(String, u64)>), crate::persist::PersistError> {
    let io =
        |err| crate::persist::PersistError::io("copying the system journal to the backup", err);
    let (copied, mut warnings) = plan_copy(dir);
    let mut written = Vec::new();
    let present: Vec<u64> = list_segments(target)
        .map(|segments| segments.into_iter().map(|(first, _)| first).collect())
        .unwrap_or_default();
    if !copied.is_empty() {
        target.join(JOURNAL_DIR).create_dir_all().map_err(io)?;
    }
    for segment in &copied {
        let first = segment
            .relative
            .strip_prefix(&format!("{JOURNAL_DIR}/"))
            .and_then(|name| name.strip_suffix(&format!(".{JOURNAL_EXTENSION}")))
            .and_then(files::parse_seq)
            .unwrap_or(0);
        let path = target.join(&segment.relative);
        let source = match read_segment(&segment.source) {
            Ok(source) => source,
            Err(err) => {
                warnings.push(format!(
                    "{}: not readable ({err}); not copied (system journal)",
                    segment.relative
                ));
                continue;
            }
        };
        let result = (|| -> std::io::Result<Option<String>> {
            let source = &source[..(segment.len as usize).min(source.len())];
            if !present.contains(&first) {
                let mut out = path.create(false)?;
                out.write_all(source)?;
                out.sync()?;
                written.push((segment.relative.clone(), source.len() as u64));
                return Ok(None);
            }
            let existing = read_segment(&path)?;
            if existing.len() > source.len() || source[..existing.len()] != existing[..] {
                if existing.as_slice() != source {
                    return Ok(Some(format!(
                        "{}: the backup's segment is not a prefix of the store's; left as it is \
                         (system journal)",
                        segment.relative
                    )));
                }
                return Ok(None);
            }
            if existing.len() < source.len() {
                path.append(&source[existing.len()..])?;
                written.push((
                    segment.relative.clone(),
                    (source.len() - existing.len()) as u64,
                ));
            }
            Ok(None)
        })();
        match result.map_err(io)? {
            Some(warning) => warnings.push(warning),
            None => {}
        }
    }
    let journal = target.join(JOURNAL_DIR);
    if journal.exists() {
        journal.store_dir().sync_dir(journal.name()).map_err(io)?;
    }
    Ok((warnings, written))
}

/// The problems of the journal in `dir` as notes for `verify` (never damage).
pub(crate) fn verify_notes(dir: &DirPath) -> Vec<String> {
    let mut notes = Vec::new();
    let segments = match list_segments(dir) {
        Ok(segments) => segments,
        Err(err) => return vec![format!("{JOURNAL_DIR}: not readable ({err})")],
    };
    let count = segments.len();
    for (index, (first, path)) in segments.into_iter().enumerate() {
        let relative = path.relative_to(dir);
        match read_segment(&path) {
            Ok(bytes) => {
                let scan = scan_segment(&bytes, &mut |_| false);
                for damage in scan.damage {
                    notes.push(format!("{relative}: {damage} (system journal)"));
                }
                if scan.torn_tail && index + 1 < count {
                    notes.push(format!("{relative}: ends inside a record (system journal)"));
                }
                if let Some(header) = scan.header
                    && header.first_seq != first
                {
                    notes.push(format!(
                        "{relative}: its header names seq {} (system journal)",
                        header.first_seq
                    ));
                }
            }
            Err(err) => notes.push(format!("{relative}: not readable ({err}) (system journal)")),
        }
    }
    notes
}

// ---------------------------------------------------------------------------------------------
// The Store's API
// ---------------------------------------------------------------------------------------------

impl<S: crate::graph::PersistentStorage> super::Store<S> {
    /// The store's system journal (book page "System Journal"), also the graph's
    /// ([`Graph::system_journal`](crate::graph::Graph::system_journal)). A writable store
    /// writes it to `journal/`; a read-only one (a backup, maintenance mode, a store of newer
    /// critical content) reads its stored records and keeps new ones in memory.
    pub fn journal(&self) -> Result<Arc<SystemJournal>, crate::persist::PersistError> {
        Ok(Arc::clone(&self.core.shared.system_journal))
    }
}

impl super::Store {
    /// A page of the system journal of the store in `dir` without opening it: no lock, nothing
    /// written (a store open elsewhere is read as its files are; `graphersal store journal`).
    /// Damaged records are skipped and named in [`JournalPage::notes`].
    pub fn journal_dir(
        dir: impl crate::persist::IntoStoreDir,
        query: &JournalQuery,
    ) -> Result<JournalPage, crate::persist::PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_newer_version(&dir)?;
        if !super::Store::is_store(dir.store_dir().clone())? && !dir.join(JOURNAL_DIR).exists() {
            return Err(crate::persist::PersistError::NotAStore {
                path: dir.display(),
                reason: "it has no GRAPH file".into(),
            });
        }
        Ok(query_dir(&dir, query)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::dir::MemDir;

    fn record(seq: u64) -> JournalRecord {
        JournalRecord {
            seq,
            origin: InstanceId::new(7, "host"),
            origin_seq: seq,
            time: 1_000 + seq as i64,
            level: Level::Notice,
            source: "store".into(),
            kind: "store.opened".into(),
            message: format!("record {seq}"),
            op: Some(3),
            parent: None,
            commit: Some(9),
            principal: Some("cli".into()),
            fields: vec![("count".into(), 5i64.into())],
        }
    }

    fn segment(records: &[JournalRecord]) -> Vec<u8> {
        let mut bytes = SegmentHeader {
            version: FORMAT_VERSION,
            instance: 7,
            first_seq: 1,
            created_at: 0,
        }
        .encode();
        for record in records {
            bytes.extend(encode_frame(record).unwrap());
        }
        bytes
    }

    #[test]
    fn a_record_round_trips() {
        let records = [record(1), record(2)];
        let scan = scan_segment(&segment(&records), &mut |_| true);
        assert_eq!(scan.records, records);
        assert!(scan.damage.is_empty() && !scan.torn_tail);
    }

    #[test]
    fn every_cut_of_the_last_record_is_a_torn_tail() {
        let bytes = segment(&[record(1), record(2)]);
        let first_end = segment(&[record(1)]).len();
        for cut in first_end + 1..bytes.len() {
            let scan = scan_segment(&bytes[..cut], &mut |_| true);
            assert!(scan.torn_tail, "cut {cut}");
            assert!(scan.damage.is_empty(), "cut {cut}: {:?}", scan.damage);
            assert_eq!(scan.records.len(), 1);
            assert_eq!(scan.valid_end, first_end as u64);
        }
    }

    #[test]
    fn a_flipped_byte_is_skipped_by_resync() {
        let bytes = segment(&[record(1), record(2), record(3)]);
        let second = segment(&[record(1)]).len();
        for offset in second..second + 30 {
            let mut damaged = bytes.clone();
            damaged[offset] ^= 0x55;
            let scan = scan_segment(&damaged, &mut |_| true);
            let seqs: Vec<u64> = scan.records.iter().map(|r| r.seq).collect();
            assert!(seqs.contains(&1) && seqs.contains(&3), "{offset}: {seqs:?}");
            assert!(!scan.damage.is_empty(), "{offset}");
        }
    }

    #[test]
    fn the_instance_file_round_trips_and_damage_regenerates_it() {
        let dir = DirPath::root(Arc::new(MemDir::new()));
        let first = instance_of(&dir, Some("alpha"), true);
        assert_eq!(first.name, "alpha");
        assert_eq!(instance_of(&dir, None, true), first);
        let renamed = instance_of(&dir, Some("beta"), true);
        assert_eq!((renamed.id, renamed.name.as_str()), (first.id, "beta"));
        files::write_atomic(&dir, INSTANCE_FILE, b"garbage").unwrap();
        assert_ne!(instance_of(&dir, None, true).id, first.id);
    }
}
