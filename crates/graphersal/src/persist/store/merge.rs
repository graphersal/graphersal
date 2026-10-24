//! The merge checkpoint (format spec 12.3): a new snapshot built from the store's newest
//! snapshot (the *base*) and the WAL after it, without the live graph and without its lock.
//!
//! **Passes.**
//! 1. *Collect*: the WAL records after the base are read (every header and body CRC checked)
//!    and the ids they touch are collected: the vertices and edges a mutation names, and the
//!    endpoints of the edges they add or drop.
//! 2. *Context*: the base snapshot's chunks that hold one of these ids (found through the
//!    manifest's chunk index, each chunk's CRC checked) are decoded, and their elements, with
//!    the endpoints of the touched edges, go into a small *delta graph* that carries the base's
//!    identity, position, schema and catalog.
//! 3. *Replay*: the same WAL records are replayed onto the delta graph by the recovery
//!    ([`Recovery`], with its commit continuity and lineage checks, and every mutation's before
//!    image checked against the delta graph: a WAL that does not continue the base is damage),
//!    exactly as a load of the base followed by a replay would apply them; the delta graph then holds the final state of
//!    every touched element (absent: dropped), the schema, the catalog, the auto-id sequences,
//!    the position, the last commit time and the lineage id.
//! 4. *Merge*: a run of touched segments is read again as a stream (every chunk CRC, the
//!    footer and the file CRC checked) and merged on the sorted id with the delta into new
//!    segments, with the two passes of the streaming encoder (`persist::encoder`): plan, then
//!    merge again and emit. A base segment no touched id falls into keeps its bytes, under the
//!    **donor rule** (format spec 12.3): the new snapshot never uses a file the base uses, so the
//!    base stays an independent donor of every id range (13.1). Such a segment is referenced
//!    (an incremental snapshot, manifest flag bit 0) when the snapshot before the base holds a
//!    *twin*, another file with the same bytes (the same manifest row and chunk rows); otherwise
//!    its bytes are copied into a new file of the new snapshot (every byte read, the file CRC
//!    checked). Unchanged ranges so alternate between two files, and only the first checkpoint
//!    after a range was rewritten copies it.
//!
//! **Memory** is proportional to the delta, never to the graph: the touched ids, the delta
//! graph, one decoded base chunk and one chunk being encoded. A delta beyond
//! [`super::StoreOptions::with_checkpoint_merge_bytes`] (measured as the WAL bytes after the
//! base, known before anything is read) is not merged: the caller falls back to the streaming
//! encoder of the loaded graph.
//!
//! **Damage** anywhere in what the merge reads (a manifest, `schema.json`, a chunk, a segment
//! file, a WAL record, a gap in the commits, a foreign lineage) stops it with the error; the
//! new snapshot is written into `snapshots/tmp-*`, verified there and renamed only when
//! everything held, so nothing visible is written. The live store keeps running.

use std::collections::BTreeSet;

use compact_str::CompactString;

use super::super::codec::{crc, crc_append};
use super::super::encoder::{
    ElementSink, KindEmitter, KindPlanner, SegmentOut, SegmentPlan, SegmentSink,
};
use super::super::error::PersistError;
use super::super::journal::DurablePosition;
use super::super::manifest::{
    ChunkRow, FLAG_INCREMENTAL, Manifest, ManifestFixed, SegmentEntry, is_element_kind,
};
use super::super::recover::{Recovery, RecoveryTarget};
use super::super::segment::{
    CHUNK_HEADER_BYTES, EdgeRecord, KIND_EDGE, KIND_VERTEX, RawChunk, SEGMENT_HEADER_BYTES,
    VertexRecord, decode_edge_chunk, decode_vertex_chunk, header_version,
};
use super::super::snapshot::{
    ReadOptions, SnapshotInfo, WriteOptions, check_ids, read_segment_stream, restore_definitions,
};
use super::super::wal::{JournalReader, JournalRecord};
use super::files::{
    DirPath, GraphFile, list_snapshots, list_wal, read_bounded, snapshot_name, wal_after,
};
use super::snapshot_dir::{
    MANIFEST_FILE, NewFile, NewSnapshotDir, SCHEMA_FILE, read_manifest, segment_path, write_file,
};
use super::wal_files::{open_segment_reader, wal_set_problems};
use crate::graph::PersistentStorage;
use crate::graph::TraversalGraph;
use crate::graph::change::Mutation;
use crate::graph::memory::{loaded_edge_bytes, loaded_vertex_bytes};
use crate::graph::schema::ElementKind;

/// The largest `schema.json` read (as for snapshot loads).
const MAX_SCHEMA_BYTES: u64 = 256 << 20;

/// What a merge checkpoint is asked to do.
pub(super) struct MergeRequest<'a> {
    pub(super) dir: &'a DirPath,
    /// The store's `GRAPH` (lineage chain, recorded WAL cut, `wal_head`, clean close).
    pub(super) graph_file: &'a GraphFile,
    /// An open store's durable end: the WAL is read up to it and never beyond (a record after
    /// it may still be cut off, format spec 9.9). `None`: a closed store, read to its end (a
    /// torn tail is ignored, a recorded WAL cut honoured, the segment set checked, 9.8).
    pub(super) durable: Option<DurablePosition>,
    pub(super) write: &'a WriteOptions,
    pub(super) read: &'a ReadOptions,
    /// The most WAL bytes after the base a merge takes on.
    pub(super) max_wal_bytes: u64,
}

/// What a merge checkpoint did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct MergeStats {
    /// The base snapshot's position.
    pub(super) base_commit_seq: u64,
    /// The commits merged into it.
    pub(super) commits: u64,
    /// The WAL bytes read after the base.
    pub(super) wal_bytes: u64,
    /// Distinct vertices and edges the commits touched.
    pub(super) touched_vertices: usize,
    pub(super) touched_edges: usize,
    /// Unchanged segments referenced (twins of the snapshot before the base), unchanged
    /// segments copied into new files (and their bytes), base segments read and replaced, new
    /// segment files written from merged data.
    pub(super) segments_reused: usize,
    pub(super) segments_copied: usize,
    pub(super) bytes_copied: u64,
    pub(super) segments_rewritten: usize,
    pub(super) segments_written: usize,
    /// The delta graph's estimated memory (`memory_usage`).
    pub(super) delta_bytes: u64,
}

/// The outcome of a merge checkpoint.
pub(super) enum Merged {
    /// A new snapshot, verified and renamed.
    Written {
        info: Box<SnapshotInfo>,
        stats: MergeStats,
    },
    /// The store has a snapshot at the position the merge would write (nothing committed
    /// after the newest one): no new snapshot.
    Current { commit_seq: u64 },
    /// The WAL after the base exceeds [`MergeRequest::max_wal_bytes`]: nothing was read.
    TooLarge { wal_bytes: u64 },
    /// The store has no snapshot to merge into.
    NoBase,
}

/// The base snapshot: its directory, manifest and schema.
struct Base {
    seq: u64,
    path: DirPath,
    manifest: Manifest,
    schema: String,
}

impl Base {
    fn open(seq: u64, path: DirPath) -> Result<Self, PersistError> {
        let (_, manifest) = read_manifest(&path)?;
        let location = format!("{}/{SCHEMA_FILE}", path.display());
        let bytes = read_bounded(&path.join(SCHEMA_FILE), MAX_SCHEMA_BYTES)
            .map_err(|err| PersistError::io("reading a snapshot's schema.json", err))?;
        if bytes.len() as u64 != manifest.fixed.schema_len
            || crc(&bytes) != manifest.fixed.schema_crc
        {
            return Err(PersistError::corrupt(
                location,
                0,
                "length or checksum differs from the manifest",
            ));
        }
        let schema = String::from_utf8(bytes)
            .map_err(|_| PersistError::corrupt(&location, 0, "not UTF-8"))?;
        if manifest.fixed.commit_seq != seq {
            return Err(PersistError::corrupt(
                format!("{}/{MANIFEST_FILE}", path.display()),
                56,
                format!(
                    "the manifest says commit {}, the directory's name {seq}",
                    manifest.fixed.commit_seq
                ),
            ));
        }
        Ok(Self {
            seq,
            path,
            manifest,
            schema,
        })
    }

    fn location(&self, entry: &SegmentEntry) -> String {
        format!("{}/{}", self.path.display(), entry.path)
    }

    fn segment(&self, number: usize) -> Result<(&SegmentEntry, DirPath), PersistError> {
        let entry = &self.manifest.segments[number];
        let path = segment_path(&self.path, &entry.path)
            .ok_or_else(|| PersistError::corrupt(self.location(entry), 0, "unsafe segment path"))?;
        Ok((entry, path))
    }
}

/// The row of segment `number` of the snapshot `seq` (manifest `manifest`) as a newer snapshot
/// references it (relative to `snapshots/`).
fn reference_row(seq: u64, manifest: &Manifest, number: usize) -> SegmentEntry {
    let mut entry = manifest.segments[number].clone();
    if !entry.path.contains('/') {
        entry.path = format!("{}/{}", snapshot_name(seq), entry.path);
    }
    entry
}

/// The snapshot before the base: where an unchanged base segment may have a twin.
pub(super) struct Previous {
    seq: u64,
    path: DirPath,
    manifest: Manifest,
}

impl Previous {
    /// The snapshot before the newest one in `snapshots` (oldest first), when its manifest
    /// reads. A manifest that does not read offers no twin: the base's bytes are copied
    /// instead, which needs nothing from it (verification reports its damage).
    pub(super) fn of(snapshots: &[(u64, DirPath)]) -> Option<Self> {
        let (seq, path) = snapshots.len().checked_sub(2).map(|i| &snapshots[i])?;
        let (_, manifest) = read_manifest(path).ok()?;
        (manifest.fixed.commit_seq == *seq).then(|| Self {
            seq: *seq,
            path: path.clone(),
            manifest,
        })
    }
}

/// The chunk index rows of segment `number` of `manifest`.
fn chunk_rows(manifest: &Manifest, number: usize) -> impl Iterator<Item = &ChunkRow> {
    manifest
        .chunks
        .iter()
        .filter(move |row| row.segment as usize == number)
}

/// A twin of segment `number` of the base (`base_seq`, `base_path`, `base`) in `previous`
/// (format spec 12.3, the donor rule): a row of the same lineage with the same kind, id range,
/// element count, length, file CRC and chunk rows, naming ANOTHER file than the base's.
/// Returned as the new snapshot references it. The new snapshot's verification reads it whole
/// before the rename.
fn twin(
    dir: &DirPath,
    (base_seq, base_path, base): (u64, &DirPath, &Manifest),
    number: usize,
    previous: Option<&Previous>,
) -> Option<SegmentEntry> {
    let previous = previous?;
    if previous.manifest.fixed.graph_id != base.fixed.graph_id || previous.seq >= base_seq {
        return None;
    }
    let row = &base.segments[number];
    let own = segment_path(base_path, &row.path)?.relative_to(dir);
    let rows: Vec<&ChunkRow> = chunk_rows(base, number).collect();
    previous
        .manifest
        .segments
        .iter()
        .enumerate()
        .find(|(n, candidate)| {
            candidate.kind == row.kind
                && candidate.first_id == row.first_id
                && candidate.last_id == row.last_id
                && candidate.count == row.count
                && candidate.byte_len == row.byte_len
                && candidate.crc == row.crc
                && chunk_rows(&previous.manifest, *n)
                    .map(|r| &r.chunk)
                    .eq(rows.iter().map(|r| &r.chunk))
                && segment_path(&previous.path, &candidate.path)
                    .is_some_and(|path| path.relative_to(dir) != own)
        })
        .map(|(n, _)| reference_row(previous.seq, &previous.manifest, n))
}

/// The WAL segments a merge reads, each with the bytes it may read (`None`: to its end).
pub(super) type Journals = Vec<(u64, DirPath, Option<u64>)>;

/// The ids the WAL after the base touches.
#[derive(Default)]
struct Touched {
    vertices: BTreeSet<CompactString>,
    edges: BTreeSet<CompactString>,
    /// Vertices the delta graph needs as edge endpoints (not changed themselves).
    endpoints: BTreeSet<CompactString>,
    /// The last commit read (the new snapshot's position).
    last: u64,
    commits: u64,
}

/// Runs a merge checkpoint (see the module documentation).
pub(super) fn merge_checkpoint(request: &MergeRequest<'_>) -> Result<Merged, PersistError> {
    let dir = request.dir;
    let snapshots = list_snapshots(dir)?;
    let Some((base_seq, base_path)) = snapshots.last().cloned() else {
        return Ok(Merged::NoBase);
    };
    if let Some(durable) = request.durable
        && durable.commit_seq <= base_seq
    {
        return Ok(Merged::Current {
            commit_seq: base_seq,
        });
    }
    let journals = journals(request, base_seq)?;
    let wal_bytes: u64 = journals
        .iter()
        .map(|(_, path, limit)| {
            let len = path.len().unwrap_or(0);
            limit.map_or(len, |limit| len.min(limit))
        })
        .sum();
    if wal_bytes > request.max_wal_bytes {
        return Ok(Merged::TooLarge { wal_bytes });
    }
    let base = Base::open(base_seq, base_path)?;
    if let Some(reason) = base.manifest.critical_segment_reason() {
        return Err(PersistError::ReadOnly {
            path: dir.display(),
            reason,
        });
    }
    let depth = request.read.max_value_depth();
    let stop = request.durable.map(|d| d.commit_seq);

    // 1. Collect.
    let touched = collect(&journals, base_seq, stop, depth)?;
    let target = touched.last.max(base_seq);
    if let Some((_, path)) = list_snapshots(dir)?
        .into_iter()
        .find(|(seq, _)| *seq == target)
    {
        // Nothing to write; a state this version must not write is still refused (format spec
        // 12.1, 16 rule 3): the snapshot at the position holds that state.
        let (_, manifest) = read_manifest(&path)?;
        let mut catalog = TraversalGraph::new();
        restore_definitions(&mut catalog, &manifest, depth)?;
        let reason = manifest
            .critical_segment_reason()
            .or_else(|| super::super::definitions::critical_unknown(catalog.definitions.iter()));
        if let Some(reason) = reason {
            return Err(PersistError::ReadOnly {
                path: dir.display(),
                reason,
            });
        }
        return Ok(Merged::Current { commit_seq: target });
    }
    super::check_snapshot_name(&super::list_snapshot_infos(dir)?, &request.write.name)?;
    if request.durable.is_none()
        && request.graph_file.clean
        && let Some(closed_at) = request.graph_file.closed_at
        && target < closed_at
    {
        return Err(PersistError::corrupt(
            dir.join(super::files::WAL_DIR).display(),
            0,
            format!(
                "the store was closed cleanly at commit {closed_at}, but its files end at commit \
                 {target}: the WAL after it is lost"
            ),
        ));
    }

    // 2. Context.
    let delta = context(&base, &touched, depth)?;

    // 3. Replay.
    let mut recovery = Recovery::new()
        .start_graph(delta)
        .verify_before_images()
        .options(request.read.clone())
        .chain(request.graph_file.chain())
        // To the end of the journals as read: they already stop at the durable end, and every
        // segment is read, also an empty one of a new lineage (a rollback without commits since
        // continues under the new lineage id).
        .target(RecoveryTarget::Latest);
    for (_, path, limit) in &journals {
        recovery = recovery.journal(open_segment_reader(path, *limit)?);
    }
    let recovered = recovery.run()?;
    let delta = recovered.graph;
    if delta.last_commit_seq() != target {
        return Err(PersistError::corrupt(
            dir.join(super::files::WAL_DIR).display(),
            0,
            format!(
                "the WAL read twice ends at different commits ({target}, then {})",
                delta.last_commit_seq()
            ),
        ));
    }
    if delta.graph_id() != request.graph_file.graph_id {
        return Err(PersistError::corrupt(
            base.path.display(),
            16,
            "the snapshot and the WAL end in another lineage than the store's GRAPH file",
        ));
    }
    if let Some(reason) = super::super::definitions::critical_unknown(delta.definitions.iter()) {
        return Err(PersistError::ReadOnly {
            path: dir.display(),
            reason,
        });
    }

    // 4. Merge.
    let mut stats = MergeStats {
        base_commit_seq: base_seq,
        commits: touched.commits,
        wal_bytes,
        touched_vertices: touched.vertices.len(),
        touched_edges: touched.edges.len(),
        delta_bytes: crate::graph::memory::compute(&delta).total_bytes(),
        ..MergeStats::default()
    };
    let previous = Previous::of(&snapshots);
    let snapshot = NewSnapshotDir::begin(dir, target)?;
    let info = write_merged(
        &Inputs {
            dir,
            base: &base,
            previous: previous.as_ref(),
            touched: &touched,
            delta: &delta,
        },
        request.write,
        depth,
        &snapshot,
        &mut stats,
    )?;
    let (_, mut written) = snapshot.finish()?;
    written.meta.clear();
    debug_assert_eq!(written.commit_seq, info.commit_seq);
    tracing::debug!(
        "merge checkpoint at commit {target}: {} commits onto snapshot {base_seq}, {} segments \
         reused, {} copied ({} bytes), {} rewritten into {}",
        stats.commits,
        stats.segments_reused,
        stats.segments_copied,
        stats.bytes_copied,
        stats.segments_rewritten,
        stats.segments_written
    );
    Ok(Merged::Written {
        info: Box::new(written),
        stats,
    })
}

/// The WAL segments after the base, with their read limits.
pub(super) fn journals(
    request: &MergeRequest<'_>,
    base_seq: u64,
) -> Result<Journals, PersistError> {
    let mut segments = list_wal(request.dir)?;
    super::drop_refused_segments(&mut segments, request.graph_file.wal_cut);
    match request.durable {
        Some(durable) => {
            // The writer's own WAL: nothing after the durable end counts.
            segments.retain(|(first, _)| *first <= durable.segment);
            Ok(wal_after(&segments, base_seq)
                .iter()
                .map(|(first, path)| {
                    let limit = (*first == durable.segment).then_some(durable.offset);
                    (*first, path.clone(), limit)
                })
                .collect())
        }
        None => {
            // A closed store: the set of segments against GRAPH first (format spec 9.8).
            if let Some((name, reason)) = wal_set_problems(&segments, request.graph_file)
                .into_iter()
                .next()
            {
                return Err(PersistError::corrupt(
                    request.dir.join(name).display(),
                    0,
                    reason,
                ));
            }
            // A last segment newer than the recorded one and shorter than its header: a crash
            // while it was being started; it holds nothing.
            if let Some((first, path)) = segments.last()
                && request.graph_file.wal_head.is_none_or(|head| *first > head)
                && path.len().unwrap_or(0) < super::super::wal::WAL_HEADER_BYTES as u64
            {
                segments.pop();
            }
            let cut = request.graph_file.wal_cut;
            Ok(wal_after(&segments, base_seq)
                .iter()
                .map(|(first, path)| (*first, path.clone(), super::cut_limit(cut, *first)))
                .collect())
        }
    }
}

/// Pass 1: the ids the commits after the base touch.
fn collect(
    journals: &Journals,
    base_seq: u64,
    stop: Option<u64>,
    depth: usize,
) -> Result<Touched, PersistError> {
    let mut touched = Touched {
        last: base_seq,
        ..Touched::default()
    };
    'journals: for (index, (_, path, limit)) in journals.iter().enumerate() {
        let mut reader = JournalReader::open(open_segment_reader(path, *limit)?, index, depth)?;
        while let Some((_, _, record)) = reader.next_record()? {
            let JournalRecord::Commit(changes) = record else {
                continue;
            };
            let seq = changes.commit_seq();
            if seq <= base_seq {
                continue;
            }
            if stop.is_some_and(|stop| seq > stop) {
                break 'journals;
            }
            touched.last = touched.last.max(seq);
            touched.commits += 1;
            for mutation in changes.mutations() {
                note(&mut touched, mutation);
            }
        }
    }
    Ok(touched)
}

/// Records what `mutation` touches.
fn note(touched: &mut Touched, mutation: &Mutation) {
    let vertex = |touched: &mut Touched, id: &str| {
        touched.vertices.insert(CompactString::from(id));
    };
    match mutation {
        Mutation::AddVertex { id, .. }
        | Mutation::DropVertex { id, .. }
        | Mutation::AddLabel { id, .. }
        | Mutation::RemoveLabel { id, .. } => vertex(touched, id),
        Mutation::AddEdge {
            id, out_id, in_id, ..
        } => {
            touched.edges.insert(CompactString::from(id.as_str()));
            touched
                .endpoints
                .insert(CompactString::from(out_id.as_str()));
            touched
                .endpoints
                .insert(CompactString::from(in_id.as_str()));
        }
        Mutation::DropEdge { id, before } => {
            touched.edges.insert(CompactString::from(id.as_str()));
            touched
                .endpoints
                .insert(CompactString::from(before.out_id.as_str()));
            touched
                .endpoints
                .insert(CompactString::from(before.in_id.as_str()));
        }
        Mutation::SetEdgeLabel { id, .. } => {
            touched.edges.insert(CompactString::from(id.as_str()));
        }
        Mutation::SetProperty { element, .. } | Mutation::RemoveProperty { element, .. } => {
            match element.kind {
                ElementKind::Vertex => vertex(touched, &element.id),
                ElementKind::Edge => {
                    touched
                        .edges
                        .insert(CompactString::from(element.id.as_str()));
                }
            }
        }
        Mutation::SetSchema { .. } | Mutation::SetDefinition { .. } => {}
    }
}

/// Reads one chunk of a base segment through the manifest's chunk index (its CRC checked).
fn read_chunk(
    base: &Base,
    row: &ChunkRow,
) -> Result<(Vec<u8>, [u8; CHUNK_HEADER_BYTES], u32), PersistError> {
    let (entry, path) = base.segment(row.segment as usize)?;
    let location = base.location(entry);
    let io = |err| PersistError::io("reading a snapshot segment", err);
    let len = path.len().map_err(io)?;
    let offset = row.chunk.offset;
    if offset.saturating_add(CHUNK_HEADER_BYTES as u64) > len {
        return Err(PersistError::corrupt(
            &location,
            offset,
            "chunk runs past the file",
        ));
    }
    let head = path.read_range(offset, CHUNK_HEADER_BYTES).map_err(io)?;
    let mut header = [0u8; CHUNK_HEADER_BYTES];
    header.copy_from_slice(&head);
    let stored_len = u64::from(u32::from_le_bytes([head[4], head[5], head[6], head[7]]));
    let start = offset + CHUNK_HEADER_BYTES as u64;
    if start.saturating_add(stored_len) > len {
        return Err(PersistError::corrupt(
            &location,
            offset,
            "chunk runs past the file",
        ));
    }
    let stored = path.read_range(start, stored_len as usize).map_err(io)?;
    let chunk_crc = u32::from_le_bytes([head[13], head[14], head[15], head[16]]);
    if crc_append(crc(&head[..13]), &stored) != chunk_crc || chunk_crc != row.chunk.crc {
        return Err(PersistError::corrupt(
            &location,
            offset,
            "chunk checksum mismatch",
        ));
    }
    let version = match path.read_range(0, SEGMENT_HEADER_BYTES) {
        Ok(head) => header_version(&head).map_err(|err| err.at(&location, 0))?,
        Err(err) => return Err(io(err)),
    };
    Ok((stored, header, version))
}

/// A chunk read by [`read_chunk`] as the decoder takes it.
fn raw_chunk<'a>(
    offset: u64,
    header: &[u8; CHUNK_HEADER_BYTES],
    stored: &'a [u8],
    version: u32,
) -> RawChunk<'a> {
    RawChunk {
        offset,
        uncompressed: u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize,
        codec: header[8],
        count: u32::from_le_bytes([header[9], header[10], header[11], header[12]]),
        crc: u32::from_le_bytes([header[13], header[14], header[15], header[16]]),
        stored,
        version,
    }
}

/// The chunk index rows of the base's segments of `kind` that may hold one of `wanted`.
fn rows_holding<'m>(
    base: &'m Base,
    kind: u8,
    wanted: &BTreeSet<CompactString>,
) -> Vec<&'m ChunkRow> {
    base.manifest
        .chunks
        .iter()
        .filter(|row| base.manifest.segments[row.segment as usize].kind == kind)
        .filter(|row| {
            row.chunk.first_id <= row.chunk.last_id
                && wanted
                    .range(row.chunk.first_id.clone()..=row.chunk.last_id.clone())
                    .next()
                    .is_some()
        })
        .collect()
}

/// Pass 2: the delta graph with the base's identity, position, schema and catalog, holding the
/// base's elements the WAL touches and the endpoints of the touched edges.
fn context(base: &Base, touched: &Touched, depth: usize) -> Result<TraversalGraph, PersistError> {
    let fixed = &base.manifest.fixed;
    let mut delta = TraversalGraph::new()
        .with_graph_id(fixed.graph_id)
        .with_last_commit_seq(fixed.commit_seq)
        .with_last_committed_at(fixed.last_commit_at)
        .with_sequences(fixed.node_sequence, fixed.edge_sequence);
    let schema_location = format!("{}/{SCHEMA_FILE}", base.path.display());
    // A schema this version cannot parse was written by a newer one (format spec 16): the
    // merge is refused, never damage.
    *delta.schema.write() = super::super::snapshot::parse_stored_schema(&base.schema, 0, None)
        .map_err(|err| err.at(schema_location, 0))?;
    restore_definitions(&mut delta, &base.manifest, depth)?;

    // Edges first: their endpoints join the vertices to load.
    let mut edges: Vec<EdgeRecord> = Vec::new();
    let mut vertices_wanted = touched.vertices.clone();
    vertices_wanted.extend(touched.endpoints.iter().cloned());
    for row in rows_holding(base, KIND_EDGE, &touched.edges) {
        let location = base.location(&base.manifest.segments[row.segment as usize]);
        let (stored, header, version) = read_chunk(base, row)?;
        let chunk = raw_chunk(row.chunk.offset, &header, &stored, version);
        let records = decode_edge_chunk(&chunk, depth).map_err(|err| err.at(&location, 0))?;
        check_ids(
            records.iter().map(|r| &r.id),
            row,
            &mut None,
            &location,
            chunk.offset,
        )?;
        for record in records {
            if touched.edges.contains(&record.id) {
                vertices_wanted.insert(record.out_id.clone());
                vertices_wanted.insert(record.in_id.clone());
                edges.push(record);
            }
        }
    }
    for row in rows_holding(base, KIND_VERTEX, &vertices_wanted) {
        let location = base.location(&base.manifest.segments[row.segment as usize]);
        let (stored, header, version) = read_chunk(base, row)?;
        let chunk = raw_chunk(row.chunk.offset, &header, &stored, version);
        let records = decode_vertex_chunk(&chunk, depth).map_err(|err| err.at(&location, 0))?;
        check_ids(
            records.iter().map(|r| &r.id),
            row,
            &mut None,
            &location,
            chunk.offset,
        )?;
        for record in records {
            if vertices_wanted.contains(&record.id) {
                check_labels(&record, &location, chunk.offset)?;
                delta.insert_vertex_raw(record.id, record.labels, record.properties);
            }
        }
    }
    for record in edges {
        let (Some(from), Some(to)) = (
            delta.index.get_node(&delta.graph, &record.out_id),
            delta.index.get_node(&delta.graph, &record.in_id),
        ) else {
            return Err(PersistError::corrupt(
                base.path.display(),
                0,
                format!("edge '{}' names a missing endpoint", record.id),
            ));
        };
        delta.insert_edge_raw(record.id, record.label, Some(record.properties), from, to);
    }
    Ok(delta)
}

/// A repeated label of a decoded vertex is damage (format spec 7.3).
fn check_labels(record: &VertexRecord, location: &str, offset: u64) -> Result<(), PersistError> {
    for (i, label) in record.labels.iter().enumerate() {
        if record.labels[..i].contains(label) {
            return Err(PersistError::corrupt(
                location,
                offset,
                format!("vertex '{}' repeats the label '{label}'", record.id),
            ));
        }
    }
    Ok(())
}

/// One entry of the new segment table of a kind: a base segment carried unchanged (referenced
/// through a twin or copied), or a run of consecutive base segments (possibly none) merged with
/// a slice of the sorted delta.
enum Item {
    Unchanged(usize),
    Run {
        rows: Vec<usize>,
        delta: std::ops::Range<usize>,
    },
}

/// The plan of one kind: which base segments stay unchanged and which are merged with which ids.
fn plan_items(base: &Base, kind: u8, delta: &[CompactString], rewrite_all: bool) -> Vec<Item> {
    let rows: Vec<usize> = base
        .manifest
        .segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == kind)
        .map(|(n, _)| n)
        .collect();
    if rows.is_empty() {
        return if delta.is_empty() {
            Vec::new()
        } else {
            vec![Item::Run {
                rows: Vec::new(),
                delta: 0..delta.len(),
            }]
        };
    }
    // Each id goes to the first segment whose range ends at or after it (the last one for ids
    // beyond every range): ranges stay ordered and disjoint.
    let segments = &base.manifest.segments;
    let assigned: Vec<usize> = delta
        .iter()
        .map(|id| {
            rows.partition_point(|&n| segments[n].last_id.as_bytes() < id.as_bytes())
                .min(rows.len() - 1)
        })
        .collect();
    let mut touched = vec![rewrite_all; rows.len()];
    for &position in &assigned {
        touched[position] = true;
    }
    let mut items = Vec::new();
    let mut position = 0;
    while position < rows.len() {
        if !touched[position] {
            items.push(Item::Unchanged(rows[position]));
            position += 1;
            continue;
        }
        let start = position;
        while position < rows.len() && touched[position] {
            position += 1;
        }
        let from = assigned.partition_point(|&a| a < start);
        let to = assigned.partition_point(|&a| a < position);
        items.push(Item::Run {
            rows: rows[start..position].to_vec(),
            delta: from..to,
        });
    }
    items
}

/// What a run's merge counted.
#[derive(Default)]
struct RunTotals {
    base_properties: u64,
    base_memory: u64,
    out_memory: u64,
}

/// Hands the final state of the delta element `id` to `sink` (nothing when it was dropped).
fn put_delta(
    kind: u8,
    id: &str,
    delta: &TraversalGraph,
    sink: &mut dyn ElementSink,
    totals: &mut RunTotals,
) -> Result<(), PersistError> {
    if kind == KIND_VERTEX {
        if let Some(vertex) = delta
            .index
            .get_node(&delta.graph, id)
            .and_then(|handle| delta.vertex_weight(handle))
        {
            totals.out_memory += loaded_vertex_bytes(&vertex.0, &vertex.1, &vertex.2);
            sink.vertex(&vertex.0, &vertex.1, &vertex.2)?;
        }
        return Ok(());
    }
    let Some(handle) = delta.index.get_edge(&delta.graph, id) else {
        return Ok(());
    };
    let (Some(edge), Some((from, to))) = (
        delta.edge_weight(handle),
        delta.graph.edge_endpoints(handle.index()),
    ) else {
        return Ok(());
    };
    let (Some(out_v), Some(in_v)) = (delta.graph.node_weight(from), delta.graph.node_weight(to))
    else {
        return Err(PersistError::encode(
            format!("edge '{id}'"),
            "an endpoint of the merged edge is missing",
        ));
    };
    totals.out_memory += loaded_edge_bytes(&edge.0, edge.1.as_deref(), edge.2.as_ref());
    sink.edge(
        &edge.0,
        edge.1.as_deref(),
        &out_v.0,
        &in_v.0,
        edge.2.as_ref(),
    )
}

/// The merge of a run's base records with its slice of the sorted delta ids.
struct RunMerge<'a> {
    kind: u8,
    ids: &'a [CompactString],
    next: usize,
    delta: &'a TraversalGraph,
    totals: RunTotals,
}

impl RunMerge<'_> {
    /// Hands `sink` every delta element before the base record `id`; returns whether the
    /// delta replaces the base record (then its final state, if any, went to `sink` too).
    fn up_to(&mut self, id: &str, sink: &mut dyn ElementSink) -> Result<bool, PersistError> {
        while self.next < self.ids.len() && self.ids[self.next].as_bytes() < id.as_bytes() {
            put_delta(
                self.kind,
                &self.ids[self.next],
                self.delta,
                sink,
                &mut self.totals,
            )?;
            self.next += 1;
        }
        if self.next < self.ids.len() && self.ids[self.next] == id {
            put_delta(
                self.kind,
                &self.ids[self.next],
                self.delta,
                sink,
                &mut self.totals,
            )?;
            self.next += 1;
            return Ok(true);
        }
        Ok(false)
    }

    /// Hands `sink` the delta elements after the last base record.
    fn rest(&mut self, sink: &mut dyn ElementSink) -> Result<(), PersistError> {
        while self.next < self.ids.len() {
            put_delta(
                self.kind,
                &self.ids[self.next],
                self.delta,
                sink,
                &mut self.totals,
            )?;
            self.next += 1;
        }
        Ok(())
    }

    /// One decoded chunk of a base segment.
    fn chunk(
        &mut self,
        row: &ChunkRow,
        chunk: &RawChunk<'_>,
        depth: usize,
        location: &str,
        last: &mut Option<CompactString>,
        sink: &mut dyn ElementSink,
    ) -> Result<(u64, Option<CompactString>), PersistError> {
        if self.kind == KIND_VERTEX {
            let records = decode_vertex_chunk(chunk, depth).map_err(|err| err.at(location, 0))?;
            check_ids(
                records.iter().map(|r| &r.id),
                row,
                last,
                location,
                chunk.offset,
            )?;
            for record in &records {
                check_labels(record, location, chunk.offset)?;
                let memory = loaded_vertex_bytes(&record.id, &record.labels, &record.properties);
                self.totals.base_properties += record.properties.len() as u64;
                self.totals.base_memory += memory;
                if !self.up_to(&record.id, sink)? {
                    self.totals.out_memory += memory;
                    sink.vertex(&record.id, &record.labels, &record.properties)?;
                }
            }
            Ok((records.len() as u64, records.first().map(|r| r.id.clone())))
        } else {
            let records = decode_edge_chunk(chunk, depth).map_err(|err| err.at(location, 0))?;
            check_ids(
                records.iter().map(|r| &r.id),
                row,
                last,
                location,
                chunk.offset,
            )?;
            for record in &records {
                let label = record.label.as_deref();
                let memory = loaded_edge_bytes(&record.id, label, Some(&record.properties));
                self.totals.base_properties += record.properties.len() as u64;
                self.totals.base_memory += memory;
                if !self.up_to(&record.id, sink)? {
                    self.totals.out_memory += memory;
                    sink.edge(
                        &record.id,
                        label,
                        &record.out_id,
                        &record.in_id,
                        Some(&record.properties),
                    )?;
                }
            }
            Ok((records.len() as u64, records.first().map(|r| r.id.clone())))
        }
    }
}

/// Streams a run: its base segments (every chunk checked and decoded, one at a time) merged on
/// the id with the run's slice of the sorted delta, into `sink`. Run twice, with the planner
/// and with the emitter, it hands both the same elements.
fn merge_run(
    base: &Base,
    kind: u8,
    rows: &[usize],
    ids: &[CompactString],
    delta: &TraversalGraph,
    depth: usize,
    sink: &mut dyn ElementSink,
) -> Result<RunTotals, PersistError> {
    let mut merge = RunMerge {
        kind,
        ids,
        next: 0,
        delta,
        totals: RunTotals::default(),
    };
    let mut last: Option<CompactString> = None;
    for &number in rows {
        let (entry, path) = base.segment(number)?;
        let location = base.location(entry);
        let mut reader = path
            .open_read()
            .map_err(|err| PersistError::io("reading a snapshot segment", err))?;
        let mut count = 0u64;
        let mut first_seen: Option<CompactString> = None;
        read_segment_stream(
            &base.manifest,
            number,
            &mut reader,
            &location,
            &mut |row, chunk| {
                let (records, first) =
                    merge.chunk(row, chunk, depth, &location, &mut last, sink)?;
                count += records;
                if first_seen.is_none() {
                    first_seen = first;
                }
                Ok(())
            },
        )?;
        if count != entry.count
            || first_seen.as_ref() != Some(&entry.first_id)
            || last.as_ref() != Some(&entry.last_id)
        {
            return Err(PersistError::corrupt(
                &location,
                0,
                "element count or id range differs from the manifest",
            ));
        }
    }
    merge.rest(sink)?;
    Ok(merge.totals)
}

/// The emit pass's segment files in the new snapshot's temporary directory.
struct FileSink<'d> {
    dir: &'d DirPath,
    file: Option<NewFile>,
}

impl SegmentSink for FileSink<'_> {
    fn begin(&mut self, plan: &SegmentPlan) -> Result<(), PersistError> {
        self.file = Some(NewFile::create(&self.dir.join(&plan.entry.path))?);
        Ok(())
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), PersistError> {
        match &mut self.file {
            Some(file) => file.put(bytes),
            None => Err(PersistError::encode(
                "a snapshot",
                "a segment written before it began",
            )),
        }
    }

    fn end(&mut self, _: &SegmentPlan) -> Result<(), PersistError> {
        match self.file.take() {
            Some(file) => file.finish(),
            None => Err(PersistError::encode(
                "a snapshot",
                "a segment ended before it began",
            )),
        }
    }
}

/// Copies the bytes of the base's segment `number` into `to` (a file of the new snapshot),
/// checking its length and CRC against the base's row: the donor rule's fresh copy of an
/// unchanged segment without a twin. Returns the bytes copied.
fn copy_segment(base: &Base, number: usize, to: &DirPath) -> Result<u64, PersistError> {
    let (entry, path) = base.segment(number)?;
    let location = base.location(entry);
    let io = |err| PersistError::io("copying a snapshot segment", err);
    let mut reader = path.open_read().map_err(io)?;
    let mut out = NewFile::create(to)?;
    let mut buf = vec![0u8; super::super::snapshot::COPY_BUFFER];
    let (mut len, mut value) = (0u64, 0u32);
    loop {
        let n = match std::io::Read::read(&mut reader, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(io(err)),
        };
        len += n as u64;
        if len > entry.byte_len {
            return Err(PersistError::corrupt(
                &location,
                entry.byte_len,
                "the file is longer than the manifest says",
            ));
        }
        value = crc_append(value, &buf[..n]);
        out.put(&buf[..n])?;
    }
    if len != entry.byte_len || value != entry.crc {
        return Err(PersistError::corrupt(
            &location,
            0,
            "length or checksum differs from the manifest",
        ));
    }
    out.finish()?;
    Ok(len)
}

/// What the merge's last pass reads.
struct Inputs<'a> {
    dir: &'a DirPath,
    base: &'a Base,
    previous: Option<&'a Previous>,
    touched: &'a Touched,
    delta: &'a TraversalGraph,
}

/// Pass 4: the new snapshot's files in its temporary directory (segments, `schema.json`, the
/// manifest last); returns what its manifest says.
fn write_merged(
    inputs: &Inputs<'_>,
    write: &WriteOptions,
    depth: usize,
    snapshot: &NewSnapshotDir,
    stats: &mut MergeStats,
) -> Result<SnapshotInfo, PersistError> {
    let Inputs {
        dir,
        base,
        previous,
        touched,
        delta,
    } = *inputs;
    let graph_id = delta.graph_id();
    // Every segment carries its snapshot's graph_id (format spec 7.1): a snapshot of another
    // lineage than the base's references none of the base's files.
    let rewrite_all = graph_id != base.manifest.fixed.graph_id;
    let fixed = &base.manifest.fixed;
    let mut segments: Vec<SegmentEntry> = Vec::new();
    let mut chunks: Vec<ChunkRow> = Vec::new();
    let mut counts = [0u64; 2];
    let mut properties = [0u64; 2];
    let mut run_properties = [0u64; 2];
    let mut memory = fixed.memory_bytes;
    let kinds = [
        (KIND_VERTEX, &touched.vertices, fixed.vertex_property_count),
        (KIND_EDGE, &touched.edges, fixed.edge_property_count),
    ];
    for (slot, (kind, ids, base_properties)) in kinds.into_iter().enumerate() {
        let ids: Vec<CompactString> = ids.iter().cloned().collect();
        let mut number = 0usize;
        let mut reused_properties = base_properties;
        for item in plan_items(base, kind, &ids, rewrite_all) {
            match item {
                Item::Unchanged(segment) => {
                    // The donor rule: a twin of the snapshot before the base, else a copy.
                    let twin = twin(
                        dir,
                        (base.seq, &base.path, &base.manifest),
                        segment,
                        previous,
                    );
                    let entry = match twin {
                        Some(entry) => {
                            stats.segments_reused += 1;
                            entry
                        }
                        None => {
                            let prefix = if kind == KIND_EDGE { "e" } else { "v" };
                            let mut entry = base.manifest.segments[segment].clone();
                            entry.path = format!("{prefix}-{number:06}.seg");
                            number += 1;
                            stats.bytes_copied +=
                                copy_segment(base, segment, &snapshot.tmp().join(&entry.path))?;
                            stats.segments_copied += 1;
                            entry
                        }
                    };
                    counts[slot] += entry.count;
                    let at = segments.len() as u32;
                    chunks.extend(
                        base.manifest
                            .chunks
                            .iter()
                            .filter(|row| row.segment as usize == segment)
                            .map(|row| ChunkRow {
                                segment: at,
                                chunk: row.chunk.clone(),
                            }),
                    );
                    segments.push(entry);
                }
                Item::Run { rows, delta: range } => {
                    let run_ids = &ids[range];
                    let mut planner = KindPlanner::new(kind, graph_id, write, number);
                    let totals = merge_run(base, kind, &rows, run_ids, delta, depth, &mut planner)?;
                    let planned_properties = planner.property_count;
                    let planned_elements = planner.element_count;
                    let (plans, next) = planner.finish()?;
                    let sink = FileSink {
                        dir: snapshot.tmp(),
                        file: None,
                    };
                    let mut emitter = KindEmitter::new(kind, write, &plans, sink);
                    merge_run(base, kind, &rows, run_ids, delta, depth, &mut emitter)?;
                    emitter.finish()?;
                    number = next;
                    reused_properties = reused_properties.saturating_sub(totals.base_properties);
                    run_properties[slot] += planned_properties;
                    counts[slot] += planned_elements;
                    memory = memory
                        .saturating_sub(totals.base_memory)
                        .saturating_add(totals.out_memory);
                    stats.segments_rewritten += rows.len();
                    stats.segments_written += plans.len();
                    for plan in &plans {
                        let (entry, plan_chunks) = plan.index();
                        let at = segments.len() as u32;
                        chunks.extend(
                            plan_chunks
                                .into_iter()
                                .map(|chunk| ChunkRow { segment: at, chunk }),
                        );
                        segments.push(entry);
                    }
                }
            }
        }
        properties[slot] = reused_properties + run_properties[slot];
    }
    let schema = delta.schema.read().to_json().into_bytes();
    let definitions = super::super::definitions::records_of(delta.definitions.iter())
        .map_err(|reason| PersistError::encode("the catalog of definitions", reason))?;
    let incremental = segments
        .iter()
        .any(|s| is_element_kind(s.kind) && s.path.contains('/'));
    let mut manifest = Manifest {
        fixed: ManifestFixed {
            flags: if incremental { FLAG_INCREMENTAL } else { 0 },
            graph_id,
            commit_seq: delta.last_commit_seq(),
            created_at: write
                .created_at
                .unwrap_or_else(crate::graph::undo::now_micros),
            last_commit_at: delta.last_committed_at(),
            node_sequence: delta.node_sequence,
            edge_sequence: delta.edge_sequence,
            vertex_count: counts[0],
            edge_count: counts[1],
            vertex_property_count: properties[0],
            edge_property_count: properties[1],
            memory_bytes: memory,
            schema_len: schema.len() as u64,
            schema_crc: crc(&schema),
            name: super::super::manifest::truncate_utf8(
                &write.name,
                super::super::manifest::MAX_NAME_BYTES,
            )
            .to_string(),
            chunk_bytes: write.chunk_bytes,
            parent_graph_id: write.parent.0,
            parent_commit_seq: write.parent.1,
            ..ManifestFixed::default()
        },
        meta: write.meta.clone(),
        segments,
        definitions,
        chunks,
    };
    let bytes = manifest.encode();
    write_file(&snapshot.tmp().join(SCHEMA_FILE), |out| out.put(&schema))?;
    write_file(&snapshot.tmp().join(MANIFEST_FILE), |out| out.put(&bytes))?;
    Ok(SnapshotInfo::from_manifest(&manifest))
}
