//! Damage handling (format spec section 13): the scan that locates damage
//! in a store (the `DamageReport` of maintenance mode) and the salvage that builds the best
//! state from what is intact plus donors (the graph of maintenance mode and of `repair_to`).
//!
//! - **Snapshots**: every chunk is checked against the manifest's chunk index (so a damaged
//!   segment header or footer does not hide intact chunks); a damaged chunk of the snapshot the
//!   state is built on is rebuilt from a DONOR: the newest older snapshot whose chunks for that
//!   id range are intact, plus the WAL records for that range up to the damaged snapshot.
//! - **WAL**: records are read with resynchronisation on the sync marker (a valid frame header
//!   CRC and body CRC); a damaged record is skipped when a later snapshot covers its commits,
//!   otherwise the replay continues after the gap and every element whose state differs from
//!   a later record's before image is reported as DIVERGED instead of being guessed.
//! - Nothing here writes to the damaged store.

use std::collections::BTreeSet;

use compact_str::CompactString;

use super::super::codec::{crc, crc_append};
use super::super::error::PersistError;
use super::super::manifest::{ChunkRow, FIXED_BYTES, Manifest, ManifestFixed, is_element_kind};
use super::super::segment::{
    self, CHUNK_HEADER_BYTES, EdgeRecord, KIND_EDGE, KIND_VERTEX, RawChunk, VertexRecord,
};
use super::super::snapshot::ReadOptions;
use super::super::wal::{
    FRAME_BYTES, RECORD_CHECKPOINT, RECORD_COMMIT, RECORD_MARK, SYNC_MARKER, WAL_HEADER_BYTES,
    WalHeader, decode_commit_in, decode_frame_header,
};
use super::files::{
    DamagePolicy, DirPath, GraphFile, LineageEntry, STORE_VERSION, list_snapshots, list_wal,
    read_bounded, read_graph_file,
};
use super::snapshot_dir::{MANIFEST_FILE, SCHEMA_FILE, segment_path};
use crate::graph::PersistentStorage;
use crate::graph::TraversalGraph;
use crate::graph::change::{ChangeSet, ElementRef, Mutation};
use crate::graph::definition::Definition;
use crate::graph::hooks::Mark;
use crate::graph::replay::mutation_element;
use crate::graph::schema::{ElementKind, GraphSchema};

/// The largest segment file a scan reads into memory.
const MAX_SEGMENT_FILE: u64 = 1 << 34;

/// What kind of thing is damaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DamageKind {
    /// `GRAPH` / `GRAPH.copy`.
    GraphFile,
    /// A snapshot manifest that cannot be read (the snapshot is unusable).
    Manifest,
    /// One of the two copies of a manifest's fixed part.
    ManifestCopy,
    /// A snapshot's `schema.json`.
    Schema,
    /// A definition in a snapshot manifest's definitions table that does not decode (format
    /// spec 6.3); the rest of the snapshot is usable, the catalog comes from a donor.
    Definitions,
    /// A snapshot segment file's header, footer or length (its chunks are listed separately).
    Segment,
    /// A snapshot chunk: the unit of repair.
    Chunk,
    /// A WAL segment's header.
    WalHeader,
    /// WAL bytes that are not a valid record (a damaged record, up to the next valid one).
    WalRecord,
    /// Commits missing from the WAL without a damaged record (a lost file).
    WalGap,
    /// A WAL segment of a lineage that is not on the store's chain.
    Lineage,
    /// The load failed for a reason the scan could not locate further.
    Load,
    /// The backend's own records (a single-file container's table or records, format spec 14):
    /// the names or bytes they held may be missing or stale.
    Container,
}

impl DamageKind {
    /// The kind as a snake_case word (`"chunk"`, `"wal_record"`, ...).
    pub fn name(&self) -> &'static str {
        match self {
            DamageKind::GraphFile => "graph_file",
            DamageKind::Manifest => "manifest",
            DamageKind::ManifestCopy => "manifest_copy",
            DamageKind::Schema => "schema",
            DamageKind::Definitions => "definitions",
            DamageKind::Segment => "segment",
            DamageKind::Chunk => "chunk",
            DamageKind::WalHeader => "wal_header",
            DamageKind::WalRecord => "wal_record",
            DamageKind::WalGap => "wal_gap",
            DamageKind::Lineage => "lineage",
            DamageKind::Load => "load",
            DamageKind::Container => "container",
        }
    }
}

/// An id range of one element kind (a chunk's, from the manifest's chunk index).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IdRange {
    /// Vertices or edges.
    pub kind: ElementKind,
    /// The first id (bytewise order).
    pub first: String,
    /// The last id.
    pub last: String,
}

impl std::fmt::Display for IdRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            ElementKind::Vertex => "vertex",
            ElementKind::Edge => "edge",
        };
        write!(f, "{kind} ids {:?}..={:?}", self.first, self.last)
    }
}

/// Where the data of a damaged item can come from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Donor {
    /// The other copy of the same metadata (`GRAPH.copy`, the manifest's other fixed part).
    OtherCopy,
    /// Only framing or identity is damaged; the data itself is intact (a segment header or
    /// footer with intact chunks, a WAL segment header, a lineage rebuilt from the files).
    IntactData,
    /// The newest older snapshot whose chunks for the range are intact, plus the WAL records
    /// for that range up to `wal_to`.
    #[non_exhaustive]
    OlderSnapshot {
        /// The donor snapshot (commit sequence number).
        snapshot: u64,
        /// The commit the WAL replay goes up to (the damaged snapshot's position).
        wal_to: u64,
    },
    /// A later snapshot holds every commit of the damaged item: it is not needed.
    #[non_exhaustive]
    LaterSnapshot {
        /// That snapshot (commit sequence number).
        snapshot: u64,
    },
}

impl std::fmt::Display for Donor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Donor::OtherCopy => f.write_str("the other copy"),
            Donor::IntactData => f.write_str("intact data (only the framing is damaged)"),
            Donor::OlderSnapshot { snapshot, wal_to } => {
                write!(f, "snapshot {snapshot} plus the WAL up to commit {wal_to}")
            }
            Donor::LaterSnapshot { snapshot } => {
                write!(f, "not needed: snapshot {snapshot} covers it")
            }
        }
    }
}

/// One damaged item.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Damage {
    /// What is damaged.
    pub kind: DamageKind,
    /// The file, relative to the store directory.
    pub path: String,
    /// The byte offset in it, when known.
    pub offset: Option<u64>,
    /// What is wrong.
    pub reason: String,
    /// The snapshot it belongs to (snapshot files).
    pub snapshot: Option<u64>,
    /// The element ids it holds (chunks, segments).
    pub ids: Option<IdRange>,
    /// The commits lost with it, `(first, last)`; `last` is `u64::MAX` when unknown (damage at
    /// the end of the WAL). `None` for a WAL record that held no commit (a mark, a checkpoint).
    pub commits: Option<(u64, u64)>,
    /// Where its data can come from (`None`: nowhere; it is lost).
    pub donor: Option<Donor>,
}

impl Damage {
    fn new(kind: DamageKind, path: impl Into<String>, reason: impl Into<String>) -> Self {
        Damage {
            kind,
            path: path.into(),
            offset: None,
            reason: reason.into(),
            snapshot: None,
            ids: None,
            commits: None,
            donor: None,
        }
    }

    /// The damage of a load failure the scan could not locate.
    pub(super) fn load(reason: String) -> Self {
        Damage::new(DamageKind::Load, ".", reason)
    }

    /// Whether nothing is lost with it (a donor exists, or a WAL record without a commit).
    pub fn is_recoverable(&self) -> bool {
        self.donor.is_some()
            || (matches!(self.kind, DamageKind::WalRecord | DamageKind::WalHeader)
                && self.commits.is_none())
    }
}

impl std::fmt::Display for Damage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path)?;
        if let Some(offset) = self.offset {
            write!(f, " at byte {offset}")?;
        }
        write!(f, ": {}", self.reason)?;
        if let Some(ids) = &self.ids {
            write!(f, "; {ids}")?;
        }
        match self.commits {
            Some((first, u64::MAX)) => write!(f, "; commits from {first} on")?,
            Some((first, last)) if first == last => write!(f, "; commit {first}")?,
            Some((first, last)) => write!(f, "; commits {first}..={last}")?,
            None => {}
        }
        match &self.donor {
            Some(donor) => write!(f, "; donor: {donor}"),
            None if self.is_recoverable() => write!(f, "; no commit lost"),
            None => write!(f, "; no donor: LOST"),
        }
    }
}

/// What the scan of a store found ([`Store::maintenance`](super::Store::maintenance),
/// [`RepairReport::damage`]): the damaged files, chunks and records, the id ranges and
/// commits affected, and for each the donor its data can come from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DamageReport {
    /// The damaged items.
    pub damage: Vec<Damage>,
    /// The snapshot the readable state is built on (`None`: no snapshot was readable).
    pub base_snapshot: Option<u64>,
    /// The position of the readable state.
    pub commit_seq: u64,
    /// Observations (the load error that started maintenance mode, a torn tail, ...).
    pub notes: Vec<String>,
    /// `Some(N)` when both `GRAPH` copies are damaged: the lineage (the chain of lineage ids
    /// and their branch points, format spec 5.3) is then rebuilt from the snapshot manifests
    /// and the WAL segment headers, which name only the lineages from commit `N` on (the base
    /// snapshot, or the first WAL segment without one); the chain before `N` (older ancestors,
    /// their branch points and times) is approximate. A repair writes this rebuilt lineage.
    pub lineage_approximate_before: Option<u64>,
}

impl DamageReport {
    /// The text of [`Self::lineage_approximate_before`]: `"lineage before commit N is
    /// approximate (both GRAPH copies damaged)"`, or `None` when `GRAPH` was readable.
    pub fn lineage_note(&self) -> Option<String> {
        self.lineage_approximate_before.map(|seq| {
            format!("lineage before commit {seq} is approximate (both GRAPH copies damaged)")
        })
    }

    /// Whether no damage was found.
    pub fn is_empty(&self) -> bool {
        self.damage.is_empty()
    }

    /// Whether a repair loses nothing: every damaged item has a donor.
    pub fn is_recoverable(&self) -> bool {
        self.damage.iter().all(Damage::is_recoverable)
    }
}

impl std::fmt::Display for DamageReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{} damaged item(s); readable state: {} at commit {}; {}",
            self.damage.len(),
            match self.base_snapshot {
                Some(seq) => format!("snapshot {seq} + WAL"),
                None => "no snapshot readable".to_string(),
            },
            self.commit_seq,
            if self.is_recoverable() {
                "a repair loses nothing"
            } else {
                "a repair LOSES data (see the items without a donor)"
            }
        )?;
        for damage in &self.damage {
            writeln!(f, "  DAMAGE {damage}")?;
        }
        if let Some(lineage) = self.lineage_note() {
            writeln!(f, "  note: {lineage}")?;
        }
        for note in &self.notes {
            writeln!(f, "  note: {note}")?;
        }
        Ok(())
    }
}

/// An element whose state did not match the WAL after a gap or a lost range (its value is
/// what the later records made of it; check it by hand).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diverged {
    /// The element.
    pub element: ElementRef,
    /// The first commit whose before image did not match (or that could not be applied).
    pub commit_seq: u64,
    /// What did not match.
    pub reason: String,
}

/// What [`Store::repair_to`](super::Store::repair_to) built.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RepairReport {
    /// Where the new store is ([`StoreDir::location`](crate::persist::StoreDir::location)).
    pub location: String,
    /// Its lineage id (a new one; its parent is the damaged store's lineage).
    pub graph_id: u128,
    /// Its position.
    pub commit_seq: u64,
    /// What the scan of the damaged store found.
    pub damage: DamageReport,
    /// What was rebuilt from donors (one line per range).
    pub repaired: Vec<String>,
    /// Commit ranges that are lost (`u64::MAX` as the end: unknown how many).
    pub lost_commits: Vec<(u64, u64)>,
    /// Id ranges of damaged chunks without a donor: their elements are lost.
    pub lost_ranges: Vec<IdRange>,
    /// Single elements that are lost (edges whose endpoint is in a lost range, ...).
    pub lost_elements: Vec<ElementRef>,
    /// Elements whose state differs from what the WAL expected: quarantine them.
    pub diverged: Vec<Diverged>,
    /// The damaged store's marks at or before the repaired position, in order: the repaired
    /// store starts a new lineage without marks, so these names stay targets of the damaged
    /// store only (best effort: what its `marks` file or WAL still yields).
    pub marks_not_carried: Vec<Mark>,
    /// The embedding models the repaired store holds: every model of the damaged store whose
    /// files check against its manifest (format spec 4.5).
    pub models: Vec<super::StoredModel>,
    /// The models that were NOT carried because their files are damaged, with the reason:
    /// add them to the repaired store again.
    pub models_not_carried: Vec<String>,
    /// Why [`Self::marks_not_carried`] is empty although the damaged store may have marks: its
    /// `marks` file and WAL could not be read (`None`: they were read). A repair still
    /// completes then, because the damaged store's marks are not carried anyway.
    pub marks_unreadable: Option<String>,
}

impl RepairReport {
    /// Whether the repair lost nothing and nothing diverged.
    pub fn is_lossless(&self) -> bool {
        self.lost_commits.is_empty()
            && self.lost_ranges.is_empty()
            && self.lost_elements.is_empty()
            && self.diverged.is_empty()
    }
}

impl std::fmt::Display for RepairReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Repaired store {} at commit {} (graph {}): {}",
            self.location,
            self.commit_seq,
            crate::value::uuid::format_canonical(self.graph_id),
            if self.is_lossless() {
                "nothing lost"
            } else {
                "DATA LOST OR DIVERGED, see below"
            }
        )?;
        for line in &self.repaired {
            writeln!(f, "  repaired  {line}")?;
        }
        for (first, last) in &self.lost_commits {
            match *last {
                u64::MAX => writeln!(f, "  lost      commits from {first} on (unknown count)")?,
                last if last == *first => writeln!(f, "  lost      commit {first}")?,
                last => writeln!(f, "  lost      commits {first}..={last}")?,
            }
        }
        for range in &self.lost_ranges {
            writeln!(f, "  lost      {range}")?;
        }
        for element in &self.lost_elements {
            writeln!(f, "  lost      {}", element_text(element))?;
        }
        for diverged in &self.diverged {
            writeln!(
                f,
                "  diverged  {} at commit {}: {}",
                element_text(&diverged.element),
                diverged.commit_seq,
                diverged.reason
            )?;
        }
        super::rollback::write_marks_not_carried(f, &self.marks_not_carried)?;
        if let Some(reason) = &self.marks_unreadable {
            writeln!(f, "  marks not listed: {reason}")?;
        }
        super::rollback::write_models_carried(f, &self.models)?;
        for reason in &self.models_not_carried {
            writeln!(f, "  model not carried: {reason}")?;
        }
        Ok(())
    }
}

fn element_text(element: &ElementRef) -> String {
    match element.kind {
        ElementKind::Vertex => format!("vertex {:?}", element.id),
        ElementKind::Edge => format!("edge {:?}", element.id),
    }
}

fn kind_of(byte: u8) -> ElementKind {
    if byte == KIND_VERTEX {
        ElementKind::Vertex
    } else {
        ElementKind::Edge
    }
}

fn relative(dir: &DirPath, path: &DirPath) -> String {
    path.relative_to(dir)
}

// ---------------------------------------------------------------------------------------------
// Snapshot scan
// ---------------------------------------------------------------------------------------------

/// One chunk of a scanned snapshot.
struct ChunkScan {
    segment: usize,
    row: ChunkRow,
    kind: u8,
    /// `Ok`: its CRC matches the chunk index.
    ok: Result<(), String>,
    /// The damage entry of a damaged chunk.
    damage: Option<usize>,
}

/// A snapshot whose manifest is readable.
struct SnapScan {
    seq: u64,
    path: DirPath,
    manifest: Manifest,
    schema: Option<GraphSchema>,
    schema_damage: Option<usize>,
    /// The decoded catalog (`None`: a definition does not decode).
    definitions: Option<Vec<Definition>>,
    definitions_damage: Option<usize>,
    chunks: Vec<ChunkScan>,
}

/// The raw chunk at `row` of a segment's bytes, checked against the chunk index.
fn chunk_at<'b>(bytes: &'b [u8], row: &ChunkRow) -> Result<RawChunk<'b>, String> {
    let offset = usize::try_from(row.chunk.offset).map_err(|_| "offset out of range")?;
    let head = bytes
        .get(offset..offset.saturating_add(CHUNK_HEADER_BYTES))
        .ok_or("the chunk lies past the end of the file")?;
    let word = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    let stored_len = word(4) as usize;
    let start = offset + CHUNK_HEADER_BYTES;
    let stored = bytes
        .get(start..start.saturating_add(stored_len))
        .ok_or("the chunk runs past the end of the file")?;
    if crc_append(crc(&head[..13]), stored) != row.chunk.crc || word(13) != row.chunk.crc {
        return Err("chunk checksum mismatch".into());
    }
    Ok(RawChunk {
        offset: row.chunk.offset,
        uncompressed: word(0) as usize,
        codec: head[8],
        count: u32::from_le_bytes([head[9], head[10], head[11], head[12]]),
        crc: row.chunk.crc,
        stored,
        version: segment::header_version(bytes).map_err(|err| err.reason)?,
    })
}

fn scan_snapshot(
    dir: &DirPath,
    seq: u64,
    path: &DirPath,
    max_depth: usize,
    damage: &mut Vec<Damage>,
) -> Result<Option<SnapScan>, PersistError> {
    let rel = relative(dir, path);
    let manifest_rel = format!("{rel}/{MANIFEST_FILE}");
    let bytes = match read_bounded(&path.join(MANIFEST_FILE), 256 << 20) {
        Ok(bytes) => bytes,
        Err(err) => {
            let mut item = Damage::new(DamageKind::Manifest, &manifest_rel, err.to_string());
            item.snapshot = Some(seq);
            damage.push(item);
            return Ok(None);
        }
    };
    if bytes.len() >= 2 * FIXED_BYTES + 4 {
        let head = ManifestFixed::decode(&bytes[..FIXED_BYTES]);
        let tail_at = bytes.len() - FIXED_BYTES;
        let tail = ManifestFixed::decode(&bytes[tail_at..]);
        let copy = |offset: u64, reason: String| {
            let mut item = Damage::new(DamageKind::ManifestCopy, &manifest_rel, reason);
            item.offset = Some(offset);
            item.snapshot = Some(seq);
            item.donor = Some(Donor::OtherCopy);
            item
        };
        match (head, tail) {
            (Ok(head), Ok(tail)) if head != tail => {
                damage.push(copy(0, "the two copies of the fixed part differ".into()))
            }
            (Err(err), Ok(_)) => {
                damage.push(copy(err.offset, format!("head copy: {}", err.reason)))
            }
            (Ok(_), Err(err)) => damage.push(copy(
                tail_at as u64 + err.offset,
                format!("tail copy: {}", err.reason),
            )),
            _ => {}
        }
    }
    let manifest = match Manifest::decode(&bytes) {
        Ok(manifest) => manifest,
        Err(err) => {
            let mut item = Damage::new(
                DamageKind::Manifest,
                &manifest_rel,
                format!("{} (the snapshot is unusable)", err.reason),
            );
            item.offset = Some(err.offset);
            item.snapshot = Some(seq);
            damage.push(item);
            return Ok(None);
        }
    };
    if manifest.fixed.commit_seq != seq {
        let mut item = Damage::new(
            DamageKind::Manifest,
            &manifest_rel,
            format!(
                "it says commit {}, the directory {seq} (the snapshot is unusable)",
                manifest.fixed.commit_seq
            ),
        );
        item.snapshot = Some(seq);
        damage.push(item);
        return Ok(None);
    }

    let schema_rel = format!("{rel}/{SCHEMA_FILE}");
    let schema = read_bounded(&path.join(SCHEMA_FILE), 256 << 20)
        .map_err(|err| err.to_string())
        .and_then(|bytes| {
            if bytes.len() as u64 != manifest.fixed.schema_len
                || crc(&bytes) != manifest.fixed.schema_crc
            {
                return Err("length or checksum differs from the manifest".to_string());
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| "not UTF-8".to_string())?;
            Ok(text.to_string())
        });
    // The checksum held: a schema this version cannot parse was written by a newer one
    // (format spec 16). Salvaging without it would drop it: refused, never damage.
    let schema = match schema {
        Ok(text) => Ok(super::super::snapshot::parse_stored_schema(&text, 0, None)
            .map_err(|err| err.at(&schema_rel, 0))?),
        Err(reason) => Err(reason),
    };
    let (schema, schema_damage) = match schema {
        Ok(schema) => (Some(schema), None),
        Err(reason) => {
            let mut item = Damage::new(DamageKind::Schema, schema_rel, reason);
            item.snapshot = Some(seq);
            damage.push(item);
            (None, Some(damage.len() - 1))
        }
    };

    let decoded: Result<Vec<Definition>, String> = manifest
        .definitions
        .iter()
        .map(|record| {
            record
                .decode_in(max_depth, manifest.fixed.version.0)
                .map_err(|err| {
                    format!(
                        "definition '{}' of kind {}: {} (payload byte {})",
                        record.name, record.kind, err.reason, err.offset
                    )
                })
        })
        .collect();
    let (definitions, definitions_damage) = match decoded {
        Ok(definitions) => (Some(definitions), None),
        Err(reason) => {
            let mut item = Damage::new(DamageKind::Definitions, &manifest_rel, reason);
            item.snapshot = Some(seq);
            damage.push(item);
            (None, Some(damage.len() - 1))
        }
    };

    let mut chunks = Vec::new();
    for (number, entry) in manifest.segments.iter().enumerate() {
        let segment_rel = format!("{rel}/{}", entry.path);
        let bytes = segment_path(path, &entry.path)
            .ok_or_else(|| "unsafe segment path".to_string())
            .and_then(|p| read_bounded(&p, MAX_SEGMENT_FILE).map_err(|err| err.to_string()));
        let rows: Vec<&ChunkRow> = manifest
            .chunks
            .iter()
            .filter(|row| row.segment as usize == number)
            .collect();
        let mut all_ok = true;
        for row in rows {
            let ok = match &bytes {
                Ok(bytes) => match chunk_at(bytes, row) {
                    // An intact chunk of a codec this build lacks (zstd without
                    // `persist-zstd`, format spec 2): unsupported, never damage, never salvaged.
                    Ok(raw) if super::super::codec::codec_unavailable(raw.codec) => {
                        return Err(PersistError::unsupported_content(
                            &segment_rel,
                            row.chunk.offset,
                            "a zstd chunk (codec 2): this build has no `persist-zstd` feature",
                        ));
                    }
                    Ok(_) => Ok(()),
                    Err(reason) => Err(reason),
                },
                Err(err) => Err(format!("the segment file is unreadable ({err})")),
            };
            let damage_index = match &ok {
                Ok(()) => None,
                Err(reason) => {
                    all_ok = false;
                    let mut item = Damage::new(DamageKind::Chunk, &segment_rel, reason.clone());
                    item.offset = Some(row.chunk.offset);
                    item.snapshot = Some(seq);
                    item.ids = Some(IdRange {
                        kind: kind_of(entry.kind),
                        first: row.chunk.first_id.to_string(),
                        last: row.chunk.last_id.to_string(),
                    });
                    damage.push(item);
                    Some(damage.len() - 1)
                }
            };
            chunks.push(ChunkScan {
                segment: number,
                row: row.clone(),
                kind: entry.kind,
                ok,
                damage: damage_index,
            });
        }
        if let Ok(bytes) = &bytes
            && all_ok
            && (bytes.len() as u64 != entry.byte_len || crc(bytes) != entry.crc)
        {
            let reason = if is_element_kind(entry.kind) {
                "the segment's header, footer or length is damaged; every chunk is intact"
            } else {
                "the file's length or checksum differs from the manifest"
            };
            let mut item = Damage::new(DamageKind::Segment, &segment_rel, reason);
            item.snapshot = Some(seq);
            // A file of another kind (format spec 7.5) holds no elements; this version neither
            // reads nor carries it, so its damage loses nothing it builds.
            if is_element_kind(entry.kind) {
                item.ids = Some(IdRange {
                    kind: kind_of(entry.kind),
                    first: entry.first_id.to_string(),
                    last: entry.last_id.to_string(),
                });
                item.donor = Some(Donor::IntactData);
            }
            damage.push(item);
        }
    }
    Ok(Some(SnapScan {
        seq,
        path: path.clone(),
        manifest,
        schema,
        schema_damage,
        definitions,
        definitions_damage,
        chunks,
    }))
}

/// Decodes the chunks of `scan` that `select` picks (intact ones only), segment by segment.
fn decode_chunks(
    scan: &SnapScan,
    select: impl Fn(&ChunkScan) -> bool,
    max_depth: usize,
    mut vertex: impl FnMut(VertexRecord),
    mut edge: impl FnMut(EdgeRecord),
) -> Result<(), String> {
    let mut current: Option<(usize, Vec<u8>)> = None;
    for chunk in scan.chunks.iter().filter(|c| c.ok.is_ok() && select(c)) {
        if current.as_ref().is_none_or(|(n, _)| *n != chunk.segment) {
            let entry = &scan.manifest.segments[chunk.segment];
            let path = segment_path(&scan.path, &entry.path).ok_or("unsafe segment path")?;
            let bytes = read_bounded(&path, MAX_SEGMENT_FILE).map_err(|err| err.to_string())?;
            current = Some((chunk.segment, bytes));
        }
        let Some((_, bytes)) = &current else {
            continue;
        };
        let raw = chunk_at(bytes, &chunk.row)?;
        if chunk.kind == KIND_VERTEX {
            for record in segment::decode_vertex_chunk(&raw, max_depth).map_err(|e| e.reason)? {
                vertex(record);
            }
        } else {
            for record in segment::decode_edge_chunk(&raw, max_depth).map_err(|e| e.reason)? {
                edge(record);
            }
        }
    }
    Ok(())
}

fn in_range(id: &str, first: &str, last: &str) -> bool {
    first <= id && id <= last
}

// ---------------------------------------------------------------------------------------------
// WAL scan
// ---------------------------------------------------------------------------------------------

/// A valid commit record of the WAL.
#[derive(Clone, Copy)]
struct CommitPos {
    seq: u64,
    segment: usize,
    offset: usize,
    len: usize,
}

/// One event of the WAL in order: a commit, or a damaged region.
enum WalEvent {
    Commit(CommitPos),
    Damaged(usize),
}

struct WalScan {
    segments: Vec<(u64, DirPath)>,
    events: Vec<WalEvent>,
    /// Lineage ids seen in segment headers, in order of appearance.
    lineages: Vec<(u128, u64)>,
}

impl WalScan {
    fn commits(&self) -> impl Iterator<Item = &CommitPos> {
        self.events.iter().filter_map(|e| match e {
            WalEvent::Commit(c) => Some(c),
            WalEvent::Damaged(_) => None,
        })
    }

    /// Decodes the commits in `(after, up_to]` in order and hands them to `f`.
    fn for_each_commit(
        &self,
        after: u64,
        up_to: u64,
        max_depth: usize,
        mut f: impl FnMut(ChangeSet),
    ) -> Result<(), String> {
        let mut current: Option<(usize, Vec<u8>)> = None;
        let mut last = after;
        for commit in self.commits() {
            if commit.seq <= last || commit.seq > up_to {
                continue;
            }
            if current.as_ref().is_none_or(|(n, _)| *n != commit.segment) {
                let path = &self.segments[commit.segment].1;
                let bytes = read_bounded(path, u64::MAX).map_err(|err| err.to_string())?;
                current = Some((commit.segment, bytes));
            }
            let Some((_, bytes)) = &current else {
                continue;
            };
            let payload = bytes
                .get(commit.offset + FRAME_BYTES..commit.offset + FRAME_BYTES + commit.len)
                .ok_or("a WAL segment shrank while it was read")?;
            let version = WalHeader::records_version(bytes);
            f(decode_commit_in(payload, max_depth, version, None).map_err(|e| e.reason)?);
            last = commit.seq;
        }
        Ok(())
    }

    /// Whether every commit in `(after, up_to]` is present.
    fn has_all(&self, after: u64, up_to: u64) -> bool {
        let mut next = after + 1;
        for commit in self.commits() {
            if commit.seq == next && next <= up_to {
                next += 1;
            }
        }
        next > up_to
    }
}

/// The result of reading one frame at `at`.
enum Frame {
    Record { len: usize, seq: u64, kind: u8 },
    Invalid(&'static str),
}

fn frame_at(bytes: &[u8], at: usize) -> Frame {
    let Some(head) = bytes.get(at..at + FRAME_BYTES) else {
        return Frame::Invalid("an incomplete record frame");
    };
    let mut frame = [0u8; FRAME_BYTES];
    frame.copy_from_slice(head);
    let Ok(header) = decode_frame_header(&frame) else {
        return Frame::Invalid("record header damaged (sync marker or header checksum)");
    };
    let len = header.length as usize;
    let Some(body) = bytes.get(at + FRAME_BYTES..at + FRAME_BYTES + len) else {
        return Frame::Invalid("the record runs past the end of the segment");
    };
    if crc(body) != header.body_crc {
        return Frame::Invalid("record checksum mismatch");
    }
    Frame::Record {
        len,
        seq: header.commit_seq,
        kind: header.record_type,
    }
}

/// Whether the bytes from `at` are an interrupted last write (format spec 9.6).
fn is_torn_tail(bytes: &[u8], at: usize) -> bool {
    let rest = &bytes[at.min(bytes.len())..];
    if rest.iter().all(|&b| b == 0) {
        return true;
    }
    if rest.len() < FRAME_BYTES {
        let marker = SYNC_MARKER.to_le_bytes();
        let prefix = rest.len().min(4);
        return rest[..prefix] == marker[..prefix];
    }
    let mut frame = [0u8; FRAME_BYTES];
    frame.copy_from_slice(&rest[..FRAME_BYTES]);
    decode_frame_header(&frame)
        .is_ok_and(|header| FRAME_BYTES + header.length as usize > rest.len())
}

fn scan_wal(
    dir: &DirPath,
    chain: Option<&GraphFile>,
    max_depth: usize,
    damage: &mut Vec<Damage>,
    notes: &mut Vec<String>,
) -> Result<WalScan, PersistError> {
    let mut segments = list_wal(dir)?;
    if let Some(cut) = chain.and_then(|c| c.wal_cut) {
        super::drop_refused_segments(&mut segments, Some(cut));
    }
    let mut scan = WalScan {
        segments: Vec::new(),
        events: Vec::new(),
        lineages: Vec::new(),
    };
    let count = segments.len();
    for (index, (first, path)) in segments.into_iter().enumerate() {
        let rel = relative(dir, &path);
        let last_segment = index + 1 == count;
        let mut bytes = match read_bounded(&path, MAX_SEGMENT_FILE) {
            Ok(bytes) => bytes,
            Err(err) => {
                damage.push(Damage::new(DamageKind::WalHeader, &rel, err.to_string()));
                continue;
            }
        };
        if let Some((segment, offset)) = chain.and_then(|c| c.wal_cut)
            && segment == first
        {
            bytes.truncate(offset as usize);
        }
        if bytes.len() < WAL_HEADER_BYTES {
            if !last_segment {
                damage.push(Damage::new(
                    DamageKind::WalHeader,
                    &rel,
                    "the segment is shorter than its header",
                ));
            }
            continue;
        }
        match WalHeader::decode(&bytes[..WAL_HEADER_BYTES]) {
            Ok(header) => {
                if chain.is_some_and(|c| !c.on_chain(header.graph_id)) {
                    let mut item = Damage::new(
                        DamageKind::Lineage,
                        &rel,
                        "a lineage that is not on the store's chain; its records are ignored",
                    );
                    item.offset = Some(12);
                    damage.push(item);
                    continue;
                }
                if scan.lineages.last().map(|(id, _)| *id) != Some(header.graph_id) {
                    scan.lineages.push((header.graph_id, first));
                }
            }
            Err(err) => {
                let mut item = Damage::new(
                    DamageKind::WalHeader,
                    &rel,
                    format!("{}; the records are read without it", err.reason),
                );
                item.offset = Some(err.offset);
                item.donor = Some(Donor::IntactData);
                damage.push(item);
            }
        }
        let version = WalHeader::records_version(&bytes);
        let segment = scan.segments.len();
        scan.segments.push((first, path));
        let mut at = WAL_HEADER_BYTES;
        while at < bytes.len() {
            match frame_at(&bytes, at) {
                Frame::Record { len, seq, kind } => {
                    if kind == RECORD_COMMIT {
                        let payload = &bytes[at + FRAME_BYTES..at + FRAME_BYTES + len];
                        match decode_commit_in(payload, max_depth, version, None) {
                            // The frame's checksums held and its semantic content (a schema)
                            // was written by a newer version (format spec 16). Salvaging
                            // around it would drop it: refused, never damage.
                            Err(err) if err.unsupported => {
                                return Err(PersistError::unsupported_content(
                                    rel,
                                    (at + FRAME_BYTES) as u64 + err.offset,
                                    format!("the record of commit {seq}: {}", err.reason),
                                ));
                            }
                            Ok(changes) if changes.commit_seq() == seq => {
                                scan.events.push(WalEvent::Commit(CommitPos {
                                    seq,
                                    segment,
                                    offset: at,
                                    len,
                                }));
                            }
                            _ => {
                                let mut item = Damage::new(
                                    DamageKind::WalRecord,
                                    &rel,
                                    "a commit record whose payload does not decode",
                                );
                                item.offset = Some(at as u64);
                                damage.push(item);
                                scan.events.push(WalEvent::Damaged(damage.len() - 1));
                            }
                        }
                    } else if kind != RECORD_MARK && kind != RECORD_CHECKPOINT {
                        let mut item = Damage::new(
                            DamageKind::WalRecord,
                            &rel,
                            format!("unknown record type {kind}"),
                        );
                        item.offset = Some(at as u64);
                        damage.push(item);
                        scan.events.push(WalEvent::Damaged(damage.len() - 1));
                    }
                    at += FRAME_BYTES + len;
                }
                Frame::Invalid(reason) => {
                    if last_segment && is_torn_tail(&bytes, at) {
                        notes.push(format!(
                            "{rel}: an interrupted last write at byte {at} ({} bytes) is ignored",
                            bytes.len() - at
                        ));
                        break;
                    }
                    // Resynchronise: the next sync marker that starts a valid record.
                    let marker = SYNC_MARKER.to_le_bytes();
                    let next = (at + 1..bytes.len().saturating_sub(3)).find(|&q| {
                        bytes[q..q + 4] == marker
                            && matches!(frame_at(&bytes, q), Frame::Record { .. })
                    });
                    let end = next.unwrap_or(bytes.len());
                    let mut item = Damage::new(
                        DamageKind::WalRecord,
                        &rel,
                        format!("{reason}; {} bytes up to the next valid record", end - at),
                    );
                    item.offset = Some(at as u64);
                    damage.push(item);
                    scan.events.push(WalEvent::Damaged(damage.len() - 1));
                    at = end;
                }
            }
        }
    }
    Ok(scan)
}

/// The commit ranges lost with damaged WAL regions and gaps; fills the `commits` of the
/// damage entries. Returns the lost ranges.
fn lost_commits(scan: &WalScan, damage: &mut Vec<Damage>, from: u64) -> Vec<(u64, u64)> {
    let mut lost = Vec::new();
    let mut last: Option<u64> = None;
    let mut pending: Vec<usize> = Vec::new();
    for event in &scan.events {
        match event {
            WalEvent::Damaged(index) => pending.push(*index),
            WalEvent::Commit(commit) => {
                if let Some(previous) = last
                    && commit.seq <= previous
                {
                    continue;
                }
                // The first commit seen continues the base snapshot (`from`): commits between
                // them are lost too (a damaged first record after the snapshot).
                let expected = last.map_or(commit.seq.min(from.saturating_add(1)), |p| p + 1);
                if commit.seq > expected {
                    let range = (expected, commit.seq - 1);
                    if pending.is_empty() {
                        let mut item = Damage::new(
                            DamageKind::WalGap,
                            "wal",
                            format!("commits {}..={} are missing", range.0, range.1),
                        );
                        item.commits = Some(range);
                        damage.push(item);
                    }
                    for index in &pending {
                        damage[*index].commits = Some(range);
                    }
                    lost.push(range);
                }
                pending.clear();
                last = Some(commit.seq);
            }
        }
    }
    if !pending.is_empty() {
        let range = (last.map_or(from + 1, |l| l + 1), u64::MAX);
        for index in &pending {
            damage[*index].commits = Some(range);
        }
        lost.push(range);
    }
    lost
}

fn overlaps(range: (u64, u64), after: u64, up_to: u64) -> bool {
    range.0 <= up_to && range.1 > after
}

// ---------------------------------------------------------------------------------------------
// Salvage
// ---------------------------------------------------------------------------------------------

/// The best state of a damaged (or intact) store and what the scan found.
pub(super) struct Salvage {
    pub(super) graph: TraversalGraph,
    pub(super) report: DamageReport,
    /// The store's `GRAPH` (rebuilt from the files when both copies are damaged).
    pub(super) graph_file: GraphFile,
    pub(super) repaired: Vec<String>,
    pub(super) lost_commits: Vec<(u64, u64)>,
    pub(super) lost_ranges: Vec<IdRange>,
    pub(super) lost_elements: Vec<ElementRef>,
    pub(super) diverged: Vec<Diverged>,
}

/// The elements of one id range at the base snapshot, rebuilt from a donor.
enum Rebuilt {
    Vertices(Vec<VertexRecord>),
    Edges(Vec<EdgeRecord>),
}

/// Rebuilds the elements of `kind` in `[first, last]` as of snapshot `scans[base]` from the
/// newest older snapshot whose chunks for the range are intact plus the WAL up to the base.
fn rebuild_range(
    scans: &[SnapScan],
    base: usize,
    kind: u8,
    range: (&str, &str),
    wal: &WalScan,
    lost: &[(u64, u64)],
    max_depth: usize,
) -> Result<(u64, Rebuilt), String> {
    let (first, last) = range;
    let target = scans[base].seq;
    for donor in scans[..base].iter().rev() {
        let overlapping = |c: &ChunkScan| {
            c.kind == kind
                && c.row.chunk.first_id.as_str() <= last
                && c.row.chunk.last_id.as_str() >= first
        };
        if donor.chunks.iter().any(|c| overlapping(c) && c.ok.is_err()) {
            continue;
        }
        if lost.iter().any(|r| overlaps(*r, donor.seq, target)) || !wal.has_all(donor.seq, target) {
            return Err(format!(
                "the WAL between snapshot {} and snapshot {target} is incomplete",
                donor.seq
            ));
        }
        let mut graph = TraversalGraph::new();
        let mut placeholders: BTreeSet<CompactString> = BTreeSet::new();
        let ensure =
            |graph: &mut TraversalGraph, placeholders: &mut BTreeSet<CompactString>, id: &str| {
                if graph.index.get_node(&graph.graph, id).is_none() {
                    graph.insert_vertex_raw(id.into(), Vec::new(), Default::default());
                    placeholders.insert(id.into());
                }
            };
        let mut vertices = Vec::new();
        let mut edges = Vec::new();
        decode_chunks(
            donor,
            overlapping,
            max_depth,
            |v| {
                if in_range(&v.id, first, last) {
                    vertices.push(v);
                }
            },
            |e| {
                if in_range(&e.id, first, last) {
                    edges.push(e);
                }
            },
        )?;
        for v in vertices {
            graph.insert_vertex_raw(v.id, v.labels, v.properties);
        }
        for e in edges {
            ensure(&mut graph, &mut placeholders, &e.out_id);
            ensure(&mut graph, &mut placeholders, &e.in_id);
            let (Some(from), Some(to)) = (
                graph.index.get_node(&graph.graph, &e.out_id),
                graph.index.get_node(&graph.graph, &e.in_id),
            ) else {
                continue;
            };
            graph.insert_edge_raw(e.id, e.label, Some(e.properties), from, to);
        }
        let wanted = kind_of(kind);
        let keep = |m: &Mutation| {
            mutation_element(m).is_some_and(|e| e.kind == wanted && in_range(&e.id, first, last))
        };
        let mut problems = Vec::new();
        wal.for_each_commit(donor.seq, target, max_depth, |changes| {
            for m in changes.mutations() {
                if let Mutation::AddEdge { out_id, in_id, .. } = m
                    && keep(m)
                {
                    ensure(&mut graph, &mut placeholders, out_id);
                    ensure(&mut graph, &mut placeholders, in_id);
                }
            }
            problems.extend(graph.replay_lenient(&changes, keep, false));
        })?;
        if let Some(problem) = problems.first() {
            return Err(format!(
                "replaying the WAL onto donor snapshot {} failed: {}",
                donor.seq, problem.reason
            ));
        }
        let rebuilt = if kind == KIND_VERTEX {
            Rebuilt::Vertices(
                graph
                    .graph
                    .node_weights()
                    .filter(|v| !placeholders.contains(&v.0))
                    .map(|v| VertexRecord {
                        id: v.0.clone(),
                        labels: graph.labels.set(v.1).to_vec(),
                        properties: v.2.clone(),
                    })
                    .collect(),
            )
        } else {
            let mut out = Vec::new();
            for index in graph.graph.edge_indices() {
                let (Some(e), Some((from, to))) = (
                    graph.graph.edge_weight(index),
                    graph.graph.edge_endpoints(index),
                ) else {
                    continue;
                };
                let (Some(out_v), Some(in_v)) =
                    (graph.graph.node_weight(from), graph.graph.node_weight(to))
                else {
                    continue;
                };
                out.push(EdgeRecord {
                    id: e.0.clone(),
                    label: graph.labels.name(e.1).cloned(),
                    out_id: out_v.0.clone(),
                    in_id: in_v.0.clone(),
                    properties: e.2.clone().unwrap_or_default(),
                });
            }
            Rebuilt::Edges(out)
        };
        return Ok((donor.seq, rebuilt));
    }
    Err("no older snapshot holds the range intact".into())
}

/// The schema at snapshot `scans[base]` when its `schema.json` is damaged: an older snapshot's
/// schema plus the schema changes in the WAL up to the base.
fn rebuild_schema(
    scans: &[SnapScan],
    base: usize,
    wal: &WalScan,
    lost: &[(u64, u64)],
    max_depth: usize,
) -> Option<(u64, GraphSchema)> {
    let target = scans[base].seq;
    let donor = scans[..base].iter().rev().find(|s| s.schema.is_some())?;
    if lost.iter().any(|r| overlaps(*r, donor.seq, target)) || !wal.has_all(donor.seq, target) {
        return None;
    }
    let mut schema = donor.schema.clone()?;
    wal.for_each_commit(donor.seq, target, max_depth, |changes| {
        for m in changes.mutations() {
            if let Mutation::SetSchema { after, .. } = m {
                schema = (**after).clone();
            }
        }
    })
    .ok()?;
    Some((donor.seq, schema))
}

/// The catalog at snapshot `scans[base]` when a definition of its manifest does not decode: an
/// older snapshot's catalog plus the catalog changes in the WAL up to the base (format spec
/// 13.4, as the schema).
fn rebuild_definitions(
    scans: &[SnapScan],
    base: usize,
    wal: &WalScan,
    lost: &[(u64, u64)],
    max_depth: usize,
) -> Option<(u64, Vec<Definition>)> {
    let target = scans[base].seq;
    let donor = scans[..base]
        .iter()
        .rev()
        .find(|s| s.definitions.is_some())?;
    if lost.iter().any(|r| overlaps(*r, donor.seq, target)) || !wal.has_all(donor.seq, target) {
        return None;
    }
    let mut catalog: Vec<Definition> = donor.definitions.clone()?;
    wal.for_each_commit(donor.seq, target, max_depth, |changes| {
        for m in changes.mutations() {
            if let Mutation::SetDefinition {
                kind, name, after, ..
            } = m
            {
                catalog.retain(|d| d.kind() != *kind || d.name() != name);
                if let Some(after) = after {
                    catalog.push((**after).clone());
                }
            }
        }
    })
    .ok()?;
    Some((donor.seq, catalog))
}

/// Scans the store in `dir` for damage and builds its best state (format spec 13).
pub(super) fn salvage(dir: &DirPath, read: &ReadOptions) -> Result<Salvage, PersistError> {
    let max_depth = read.max_value_depth();
    let mut damage = Vec::new();
    let mut notes = Vec::new();
    // Files of a newer (or older) format are never damage and never salvaged (format spec
    // 15): refused before anything is scanned.
    super::files::refuse_other_version(dir)?;
    // The backend's own records come first: what it lost is missing from every file below.
    let backend = crate::persist::dir::backend_check(&**dir.store_dir());
    for reason in backend.damage {
        damage.push(Damage::new(DamageKind::Container, ".", reason));
    }
    notes.extend(backend.warnings);
    let graph_file = match read_graph_file(dir) {
        Ok(read) => {
            if let Some(repaired) = read.repaired {
                let mut item = Damage::new(DamageKind::GraphFile, "GRAPH", repaired);
                item.donor = Some(Donor::OtherCopy);
                damage.push(item);
            }
            Some(read.file)
        }
        // A store of an older format is not damage: nothing to salvage.
        Err(err @ PersistError::OlderFormat { .. }) => return Err(err),
        Err(err) => {
            let mut item = Damage::new(
                DamageKind::GraphFile,
                "GRAPH",
                format!("{err}; the identity is rebuilt from the snapshots and WAL headers"),
            );
            item.donor = Some(Donor::IntactData);
            damage.push(item);
            None
        }
    };

    // Snapshots, oldest first; the usable ones.
    let mut scans = Vec::new();
    let listed = list_snapshots(dir)?;
    for (seq, path) in &listed {
        if let Some(scan) = scan_snapshot(dir, *seq, path, max_depth, &mut damage)? {
            scans.push(scan);
        }
    }
    let wal = scan_wal(dir, graph_file.as_ref(), max_depth, &mut damage, &mut notes)?;
    let base = scans.len().checked_sub(1);
    let base_seq = base.map_or(0, |b| scans[b].seq);
    let mut lost = lost_commits(&wal, &mut damage, base_seq);

    // Unusable snapshots newer than the base: rebuilt from the base plus the WAL.
    for item in damage.iter_mut() {
        if item.kind == DamageKind::Manifest
            && let Some(seq) = item.snapshot
            && seq > base_seq
            && base.is_some()
            && !lost.iter().any(|r| overlaps(*r, base_seq, seq))
            && wal.has_all(base_seq, seq)
        {
            item.donor = Some(Donor::OlderSnapshot {
                snapshot: base_seq,
                wal_to: seq,
            });
        }
    }

    let mut salvage = Salvage {
        graph: TraversalGraph::new(),
        report: DamageReport::default(),
        graph_file: GraphFile {
            version: STORE_VERSION,
            clean: false,
            graph_id: 0,
            created_at: crate::graph::undo::now_micros(),
            chunk_bytes: super::super::segment::DEFAULT_CHUNK_BYTES,
            flags: 0,
            lineage: Vec::new(),
            wal_cut: None,
            wal_head: None,
            closed_at: None,
            backup: None,
            damage_policy: DamagePolicy::default(),
            // A placeholder: a repair is a new store with its own identity.
            store_id: crate::value::uuid::new_v7(),
        },
        repaired: Vec::new(),
        lost_commits: Vec::new(),
        lost_ranges: Vec::new(),
        lost_elements: Vec::new(),
        diverged: Vec::new(),
    };

    // The base state: the newest usable snapshot, its damaged chunks rebuilt from donors.
    let mut graph = TraversalGraph::new();
    if let Some(b) = base {
        let scan = &scans[b];
        let fixed = &scan.manifest.fixed;
        graph = TraversalGraph::new()
            .with_graph_id(fixed.graph_id)
            .with_last_commit_seq(fixed.commit_seq)
            .with_last_committed_at(fixed.last_commit_at)
            .with_sequences(fixed.node_sequence, fixed.edge_sequence);
        let schema = match &scan.schema {
            Some(schema) => Some(schema.clone()),
            None => match rebuild_schema(&scans, b, &wal, &lost, max_depth) {
                Some((donor, schema)) => {
                    if let Some(index) = scan.schema_damage {
                        damage[index].donor = Some(Donor::OlderSnapshot {
                            snapshot: donor,
                            wal_to: scan.seq,
                        });
                    }
                    salvage.repaired.push(format!(
                        "the schema of snapshot {} from snapshot {donor} plus the WAL",
                        scan.seq
                    ));
                    Some(schema)
                }
                None => {
                    notes.push(format!(
                        "the schema of snapshot {} is lost: the state has no schema until the \
                         WAL sets one",
                        scan.seq
                    ));
                    None
                }
            },
        };
        if let Some(schema) = schema {
            *graph.schema.write() = schema;
        }
        let definitions = match &scan.definitions {
            Some(definitions) => Some(definitions.clone()),
            None => match rebuild_definitions(&scans, b, &wal, &lost, max_depth) {
                Some((donor, definitions)) => {
                    if let Some(index) = scan.definitions_damage {
                        damage[index].donor = Some(Donor::OlderSnapshot {
                            snapshot: donor,
                            wal_to: scan.seq,
                        });
                    }
                    salvage.repaired.push(format!(
                        "the definitions of snapshot {} from snapshot {donor} plus the WAL",
                        scan.seq
                    ));
                    Some(definitions)
                }
                None => {
                    notes.push(format!(
                        "the definitions of snapshot {} are lost: the catalog holds only what \
                         the WAL defines after it",
                        scan.seq
                    ));
                    None
                }
            },
        };
        for definition in definitions.into_iter().flatten() {
            graph.definitions.insert(definition);
        }
        // Vertices: intact chunks, then rebuilt ranges; then edges likewise.
        for kind in [KIND_VERTEX, KIND_EDGE] {
            let mut edges = Vec::new();
            let decoded = decode_chunks(
                scan,
                |c| c.kind == kind,
                max_depth,
                |v| {
                    if graph.index.get_node(&graph.graph, &v.id).is_none() {
                        graph.insert_vertex_raw(v.id, v.labels, v.properties);
                    }
                },
                |e| edges.push(e),
            );
            if let Err(reason) = decoded {
                notes.push(format!(
                    "snapshot {}: a chunk did not decode: {reason}",
                    scan.seq
                ));
            }
            for chunk in scan
                .chunks
                .iter()
                .filter(|c| c.kind == kind && c.ok.is_err())
            {
                let range = IdRange {
                    kind: kind_of(kind),
                    first: chunk.row.chunk.first_id.to_string(),
                    last: chunk.row.chunk.last_id.to_string(),
                };
                match rebuild_range(
                    &scans,
                    b,
                    kind,
                    (&range.first, &range.last),
                    &wal,
                    &lost,
                    max_depth,
                ) {
                    Ok((donor, rebuilt)) => {
                        if let Some(index) = chunk.damage {
                            damage[index].donor = Some(Donor::OlderSnapshot {
                                snapshot: donor,
                                wal_to: scan.seq,
                            });
                        }
                        let count = match rebuilt {
                            Rebuilt::Vertices(vertices) => {
                                let count = vertices.len();
                                for v in vertices {
                                    if graph.index.get_node(&graph.graph, &v.id).is_none() {
                                        graph.insert_vertex_raw(v.id, v.labels, v.properties);
                                    }
                                }
                                count
                            }
                            Rebuilt::Edges(rebuilt) => {
                                let count = rebuilt.len();
                                edges.extend(rebuilt);
                                count
                            }
                        };
                        salvage.repaired.push(format!(
                            "{range} of snapshot {} ({count} elements) from snapshot {donor} \
                             plus the WAL",
                            scan.seq
                        ));
                    }
                    Err(reason) => {
                        if let Some(index) = chunk.damage {
                            damage[index].reason = format!("{}; {reason}", damage[index].reason);
                        }
                        salvage.lost_ranges.push(range);
                    }
                }
            }
            for e in edges {
                if graph.index.get_edge(&graph.graph, &e.id).is_some() {
                    continue;
                }
                match (
                    graph.index.get_node(&graph.graph, &e.out_id),
                    graph.index.get_node(&graph.graph, &e.in_id),
                ) {
                    (Some(from), Some(to)) => {
                        graph.insert_edge_raw(e.id, e.label, Some(e.properties), from, to);
                    }
                    _ => salvage.lost_elements.push(ElementRef {
                        kind: ElementKind::Edge,
                        id: e.id.to_string(),
                    }),
                }
            }
        }
        // Compressed properties (D11): one pass after the elements and the catalog.
        graph.compression_load_pass();
        salvage.report.base_snapshot = Some(scan.seq);
        notes.push(format!(
            "built on snapshot {} ({})",
            scan.seq,
            relative(dir, &scan.path)
        ));
    } else {
        notes.push("no snapshot is readable: the state is built from the WAL alone".into());
        if let Some((id, _)) = wal.lineages.first() {
            graph = graph.with_graph_id(*id);
        }
    }

    // Damage in snapshots older than the base: covered by the base.
    for item in damage.iter_mut() {
        if item.donor.is_none() && base.is_some() && item.snapshot.is_some_and(|seq| seq < base_seq)
        {
            item.donor = Some(Donor::LaterSnapshot { snapshot: base_seq });
        }
        if (item.kind == DamageKind::WalRecord || item.kind == DamageKind::WalGap)
            && let Some((_, last)) = item.commits
            && last <= base_seq
        {
            item.donor = Some(Donor::LaterSnapshot { snapshot: base_seq });
        }
    }

    // The WAL after the base: lenient replay, before images checked.
    let mut seen: BTreeSet<(u8, String)> = BTreeSet::new();
    let mut lineage = graph.graph_id();
    let lineages = wal.lineages.clone();
    let replayed = wal.for_each_commit(base_seq, u64::MAX, max_depth, |changes| {
        // A later lineage (an in-place rollback) continues under its id.
        if let Some((id, _)) = lineages
            .iter()
            .rev()
            .find(|(_, first)| *first <= changes.commit_seq())
            && *id != lineage
        {
            lineage = *id;
        }
        for issue in graph.replay_lenient(&changes, |_| true, true) {
            let Some(element) = issue.element else {
                notes.push(format!(
                    "commit {}: a schema or catalog change did not apply as recorded and was \
                     stored anyway: {}",
                    changes.commit_seq(),
                    issue.reason
                ));
                continue;
            };
            let key = (
                u8::from(element.kind == ElementKind::Edge),
                element.id.clone(),
            );
            if seen.insert(key) {
                salvage.diverged.push(Diverged {
                    element,
                    commit_seq: changes.commit_seq(),
                    reason: issue.reason,
                });
            }
        }
    });
    if let Err(reason) = replayed {
        notes.push(format!("the WAL replay stopped: {reason}"));
    }
    if lineage != graph.graph_id() {
        graph = graph.with_graph_id(lineage);
    }
    // The set of WAL segments against GRAPH (format spec 9.8): a missing, emptied or replaced
    // newest segment, and a clean close the files no longer reach, lose the commits after
    // what was salvaged.
    if let Some(file) = graph_file.as_ref() {
        let mut segments = list_wal(dir)?;
        super::drop_refused_segments(&mut segments, file.wal_cut);
        let after = graph.last_commit_seq().saturating_add(1);
        for (name, reason) in super::wal_files::wal_set_problems(&segments, file) {
            let mut item = Damage::new(DamageKind::WalGap, &name, reason);
            item.commits = Some((after, u64::MAX));
            damage.push(item);
            lost.push((after, u64::MAX));
        }
        if file.clean
            && let Some(closed_at) = file.closed_at
            && closed_at >= after
        {
            let mut item = Damage::new(
                DamageKind::WalGap,
                "wal",
                format!(
                    "the store was closed cleanly at commit {closed_at}, but its files end at \
                     commit {}",
                    after - 1
                ),
            );
            item.commits = Some((after, closed_at));
            damage.push(item);
            lost.push((after, closed_at));
        }
    }
    salvage.lost_commits = lost
        .iter()
        .copied()
        .filter(|(_, last)| *last > base_seq)
        .collect();

    // The identity: GRAPH, or rebuilt from what the files say (then only from the base
    // snapshot, or the first WAL segment without one, on: the chain before is approximate).
    let lineage_approximate_before = graph_file.is_none().then(|| match base {
        Some(b) => scans[b].seq,
        None => wal
            .lineages
            .first()
            .map_or(graph.last_commit_seq(), |(_, first)| *first),
    });
    salvage.graph_file = match graph_file {
        Some(file) => file,
        None => {
            let mut entries: Vec<LineageEntry> = Vec::new();
            let mut ids: Vec<(u128, u64)> = Vec::new();
            if let Some(b) = base {
                let fixed = &scans[b].manifest.fixed;
                if fixed.parent_graph_id != 0 {
                    ids.push((fixed.parent_graph_id, 0));
                }
                ids.push((fixed.graph_id, fixed.parent_commit_seq));
            }
            for (id, first) in &wal.lineages {
                if !ids.iter().any(|(known, _)| known == id) {
                    ids.push((*id, first.saturating_sub(1)));
                }
            }
            for (id, branched_at) in ids.into_iter().rev() {
                entries.push(LineageEntry {
                    graph_id: id,
                    branched_at,
                    time: 0,
                });
            }
            if entries.is_empty() {
                entries.push(LineageEntry {
                    graph_id: graph.graph_id(),
                    branched_at: 0,
                    time: 0,
                });
            }
            GraphFile {
                version: STORE_VERSION,
                clean: false,
                graph_id: entries[0].graph_id,
                created_at: crate::graph::undo::now_micros(),
                chunk_bytes: base.map_or(super::super::segment::DEFAULT_CHUNK_BYTES, |b| {
                    scans[b].manifest.fixed.chunk_bytes.max(1)
                }),
                flags: 0,
                lineage: entries,
                wal_cut: None,
                wal_head: None,
                closed_at: None,
                backup: None,
                // Both GRAPH copies are lost: the policy they held is unknown, the safe
                // default applies (format spec 5.2).
                damage_policy: DamagePolicy::default(),
                // Unknown as well; a repair is a new store with its own identity anyway.
                store_id: crate::value::uuid::new_v7(),
            }
        }
    };
    salvage.report = DamageReport {
        damage,
        base_snapshot: base.map(|b| scans[b].seq),
        commit_seq: graph.last_commit_seq(),
        notes,
        lineage_approximate_before,
    };
    salvage.graph = graph;
    Ok(salvage)
}
