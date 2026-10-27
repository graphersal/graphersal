//! The streaming snapshot encoder (format spec sections 6 to 8, 12.3): a graph written as a
//! snapshot chunk by chunk, into a packed stream or into the files of a snapshot directory,
//! without ever holding more than one chunk.
//!
//! **Why two passes.** A segment file starts with a header that records its chunk count and
//! footer offset (format spec 7.1), and a packed stream starts with the manifest, which records
//! every segment's length and CRC and the chunk index (format spec 8). Both are known only once
//! every chunk is compressed, and a [`StoreDir`](super::StoreDir) file is only ever appended to
//! (never patched in place), as is a non-seekable stream. So the encoder runs twice over the
//! id-sorted elements:
//!
//! 1. **Plan.** Every chunk is encoded and compressed as it will be written, and only what the
//!    manifest and the segment framing need is kept: per chunk its element range, offset, ids
//!    and CRC; per segment its header, footer, length and CRC (the file CRC is combined from
//!    the header's and the rest's, [`crc_combine`]). The chunk's bytes are dropped at once.
//! 2. **Emit.** Each segment is written in order: its header, every chunk encoded again from
//!    its element range (chunks are self-contained, so a chunk encodes alone to the same
//!    bytes; the CRC is compared with the plan's and a difference fails the write), its footer.
//!
//! A packed stream plans every segment before it writes the manifest; a snapshot directory
//! emits each segment as soon as its plan is complete and writes the manifest last.
//!
//! **Memory.** The sorted id index (a borrowed id and a handle per element, 24 bytes), one
//! chunk being encoded (its body, string table, payload and compressed bytes) and the plans
//! (a few dozen bytes plus two ids per chunk). Nothing grows with the snapshot's size except
//! the index and the plans. The CPU cost is two encodes.
//!
//! **Any persistent storage.** The encoder reads an `S: PersistentStorage` through the storage
//! read API only: the ids are sorted as `(id, S::VertexId)` / `(id, S::EdgeId)`; labels,
//! properties and endpoints come from the trait's getters, the schema from `get_schema`, the
//! catalog from `definitions`, the position from `last_commit_seq` / `last_committed_at` /
//! `id_sequences`. There is no `TraversalGraph` fast path: on `GraphSource::large_110k()` the
//! generic walk measured within noise of the former petgraph-order walk (design
//! `book/src/for_developers/custom_storage.md`), and the golden fingerprints below pin the bytes.
//!
//! **Consistency.** The caller keeps the graph's read access for both passes (the graph must
//! not change between them); the snapshot is the graph at
//! [`GraphStorage::last_commit_seq`](crate::graph::GraphStorage::last_commit_seq).
//!
//! **Merged streams.** [`KindPlanner`] and [`KindEmitter`] run the same two passes over elements
//! handed to them one by one ([`ElementSink`]) instead of a graph's index: the merge checkpoint
//! (`store::merge`) feeds them a base snapshot's segments merged with the WAL delta, twice.

use std::cell::Cell;
use std::ops::Range;

use compact_str::CompactString;

use super::codec::{crc, crc_append, crc_combine};
use super::error::PersistError;
use super::manifest::{ChunkRow, Manifest, ManifestFixed, SegmentEntry};
use super::segment::{
    ChunkEncoder, ChunkIndexEntry, EncodedChunk, KIND_EDGE, KIND_VERTEX, SEGMENT_HEADER_BYTES,
    segment_footer, segment_header,
};
use super::snapshot::{SnapshotInfo, WriteOptions};
use crate::graph::PersistentStorage;
use crate::value::PropertyMap;

/// One chunk of a planned segment.
#[derive(Debug)]
struct ChunkPlan {
    /// The positions of its elements in the kind's sorted index.
    range: Range<usize>,
    /// Its chunk index entry (offset within the segment).
    entry: ChunkIndexEntry,
    /// Header plus stored bytes.
    byte_len: u64,
}

/// A planned segment: everything about it but its chunks' bytes.
#[derive(Debug)]
pub(crate) struct SegmentPlan {
    pub(crate) entry: SegmentEntry,
    header: [u8; SEGMENT_HEADER_BYTES],
    footer: Vec<u8>,
    chunks: Vec<ChunkPlan>,
}

impl SegmentPlan {
    /// The segment's manifest row and chunk index entries.
    pub(crate) fn index(&self) -> (SegmentEntry, Vec<ChunkIndexEntry>) {
        (
            self.entry.clone(),
            self.chunks.iter().map(|c| c.entry.clone()).collect(),
        )
    }
}

/// Where the emit pass writes a segment's bytes.
pub(crate) trait SegmentOut {
    fn put(&mut self, bytes: &[u8]) -> Result<(), PersistError>;
}

/// What the encoder measured about its own memory (crate tests).
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct EncodeStats {
    /// The largest number of bytes held for chunks at once (encoder buffers plus one finished
    /// chunk).
    pub(crate) peak_chunk_bytes: usize,
    /// The sorted id index.
    pub(crate) index_bytes: usize,
    /// Chunks encoded in both passes together.
    pub(crate) chunks_encoded: usize,
}

/// A graph being written as a snapshot: its id-sorted index and the options. Generic over the
/// storage: everything is read through the [`GraphStorage`](crate::graph::GraphStorage) read API and
/// [`PersistentStorage`] (position, lineage, schema, catalog), so every persistent storage
/// writes the same bytes for the same graph.
pub(crate) struct SnapshotEncoder<'g, S: PersistentStorage + ?Sized> {
    graph: &'g S,
    options: &'g WriteOptions,
    vertices: Vec<(&'g str, S::VertexId)>,
    edges: Vec<(&'g str, S::EdgeId)>,
    peak: Cell<usize>,
    chunks_encoded: Cell<usize>,
}

/// What the plan pass of one kind counted.
pub(crate) struct KindTotals {
    pub(crate) property_count: u64,
}

impl<'g, S: PersistentStorage + ?Sized> SnapshotEncoder<'g, S> {
    /// Sorts the graph's element ids (the only allocation that grows with the graph).
    pub(crate) fn new(graph: &'g S, options: &'g WriteOptions) -> Self {
        // Sized up front: the index is allocated once, at its final size.
        let mut vertices: Vec<(&str, S::VertexId)> = Vec::with_capacity(graph.vertex_count());
        vertices.extend(
            graph
                .vertices()
                .filter_map(|vertex| Some((graph.vertex_id(vertex)?, vertex))),
        );
        vertices.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let mut edges: Vec<(&str, S::EdgeId)> = Vec::with_capacity(graph.edge_count());
        edges.extend(
            graph
                .edges()
                .filter_map(|edge| Some((graph.edge_id(edge)?, edge))),
        );
        edges.sort_unstable_by(|a, b| a.0.cmp(b.0));
        Self {
            graph,
            options,
            vertices,
            edges,
            peak: Cell::new(0),
            chunks_encoded: Cell::new(0),
        }
    }

    pub(crate) fn stats(&self) -> EncodeStats {
        EncodeStats {
            peak_chunk_bytes: self.peak.get(),
            index_bytes: self.vertices.capacity() * size_of::<(&str, S::VertexId)>()
                + self.edges.capacity() * size_of::<(&str, S::EdgeId)>(),
            chunks_encoded: self.chunks_encoded.get(),
        }
    }

    fn len(&self, kind: u8) -> usize {
        if kind == KIND_VERTEX {
            self.vertices.len()
        } else {
            self.edges.len()
        }
    }

    /// Adds the element at `position` of the kind's index; returns the chunk it closed and the
    /// element's top-level property count.
    fn add(
        &self,
        encoder: &mut ChunkEncoder,
        kind: u8,
        position: usize,
    ) -> Result<(Option<EncodedChunk>, u64), PersistError> {
        let graph = self.graph;
        if kind == KIND_VERTEX {
            let (id, vertex) = self.vertices[position];
            let Some(properties) = graph.vertex_properties(vertex) else {
                return Ok((None, 0));
            };
            let chunk = encoder.vertex(id, graph.vertex_labels(vertex), properties)?;
            Ok((chunk, properties.len() as u64))
        } else {
            let (id, edge) = self.edges[position];
            let Some((from, to)) = graph.edge_endpoints(edge) else {
                return Ok((None, 0));
            };
            let (Some(out_id), Some(in_id)) = (graph.vertex_id(from), graph.vertex_id(to)) else {
                return Ok((None, 0));
            };
            let properties = graph.edge_properties(edge);
            let chunk = encoder.edge(id, graph.edge_label(edge), out_id, in_id, properties)?;
            Ok((chunk, properties.map_or(0, |p| p.len() as u64)))
        }
    }

    fn note(&self, encoder: &ChunkEncoder, chunk: &EncodedChunk) {
        self.chunks_encoded.set(self.chunks_encoded.get() + 1);
        let held = encoder.held_bytes() + chunk.stored.capacity() + chunk.header.len();
        if held > self.peak.get() {
            self.peak.set(held);
        }
    }

    fn chunk_encoder(&self, kind: u8) -> ChunkEncoder {
        ChunkEncoder::new(kind, self.options.codec, self.options.chunk_bytes)
    }

    /// The plan pass of one kind: encodes every chunk once, keeps its plan, and hands each
    /// segment to `segment` as soon as it is complete (with its number within the kind).
    pub(crate) fn plan_kind(
        &self,
        kind: u8,
        segment: &mut dyn FnMut(SegmentPlan) -> Result<(), PersistError>,
    ) -> Result<KindTotals, PersistError> {
        let mut encoder = self.chunk_encoder(kind);
        let mut builder = SegmentBuilder::new(kind, self.graph.graph_id(), self.options);
        let mut property_count = 0u64;
        let mut chunk_start = 0usize;
        let total = self.len(kind);
        for position in 0..total {
            let (closed, properties) = self.add(&mut encoder, kind, position)?;
            property_count += properties;
            if let Some(chunk) = closed {
                self.note(&encoder, &chunk);
                if let Some(done) = builder.push(chunk_start..position + 1, &chunk)? {
                    segment(done)?;
                }
                chunk_start = position + 1;
            }
        }
        if let Some(chunk) = encoder.finish() {
            self.note(&encoder, &chunk);
            if let Some(done) = builder.push(chunk_start..total, &chunk)? {
                segment(done)?;
            }
        }
        if let Some(done) = builder.close()? {
            segment(done)?;
        }
        Ok(KindTotals { property_count })
    }

    /// The emit pass of one planned segment: header, every chunk encoded again (and checked
    /// against the plan), footer.
    pub(crate) fn emit(
        &self,
        plan: &SegmentPlan,
        out: &mut dyn SegmentOut,
    ) -> Result<(), PersistError> {
        let kind = plan.entry.kind;
        out.put(&plan.header)?;
        let mut encoder = self.chunk_encoder(kind);
        for chunk_plan in &plan.chunks {
            let mut produced = None;
            for position in chunk_plan.range.clone() {
                if produced.is_some() {
                    return Err(changed());
                }
                produced = self.add(&mut encoder, kind, position)?.0;
            }
            let chunk = match produced {
                Some(chunk) => chunk,
                None => encoder.finish().ok_or_else(changed)?,
            };
            self.note(&encoder, &chunk);
            if chunk.crc != chunk_plan.entry.crc || chunk.byte_len() != chunk_plan.byte_len {
                return Err(changed());
            }
            out.put(&chunk.header)?;
            out.put(&chunk.stored)?;
        }
        out.put(&plan.footer)
    }

    /// The manifest of the planned segments (vertex segments first, in order).
    pub(crate) fn manifest(
        &self,
        segments: &[(SegmentEntry, Vec<ChunkIndexEntry>)],
        vertex_property_count: u64,
        edge_property_count: u64,
        schema: &[u8],
    ) -> Result<(Manifest, Vec<u8>, SnapshotInfo), PersistError> {
        let graph = self.graph;
        let options = self.options;
        let definitions = super::definitions::records_of(graph.definitions()?)
            .map_err(|reason| PersistError::encode("the catalog of definitions", reason))?;
        let (node_sequence, edge_sequence) = graph.id_sequences();
        let mut rows = Vec::new();
        for (number, (_, chunks)) in segments.iter().enumerate() {
            for chunk in chunks {
                rows.push(ChunkRow {
                    segment: number as u32,
                    chunk: chunk.clone(),
                });
            }
        }
        let mut manifest = Manifest {
            fixed: ManifestFixed {
                graph_id: graph.graph_id(),
                commit_seq: graph.last_commit_seq()?,
                created_at: options
                    .created_at
                    .unwrap_or_else(crate::graph::undo::now_micros),
                last_commit_at: graph.last_committed_at(),
                node_sequence,
                edge_sequence,
                vertex_count: graph.vertex_count() as u64,
                edge_count: graph.edge_count() as u64,
                vertex_property_count,
                edge_property_count,
                memory_bytes: graph.memory_usage()?.map_or(0, |usage| usage.total_bytes()),
                schema_len: schema.len() as u64,
                schema_crc: crc(schema),
                name: super::manifest::truncate_utf8(
                    &options.name,
                    super::manifest::MAX_NAME_BYTES,
                )
                .to_string(),
                chunk_bytes: options.chunk_bytes,
                parent_graph_id: options.parent.0,
                parent_commit_seq: options.parent.1,
                ..ManifestFixed::default()
            },
            meta: options.meta.clone(),
            segments: segments.iter().map(|(entry, _)| entry.clone()).collect(),
            definitions,
            chunks: rows,
        };
        let bytes = manifest.encode();
        let info = SnapshotInfo::from_manifest(&manifest);
        Ok((manifest, bytes, info))
    }

    /// The `schema.json` bytes of the graph (those of the empty schema when none is stored).
    pub(crate) fn schema(&self) -> Result<Vec<u8>, PersistError> {
        Ok(self
            .graph
            .get_schema()?
            .unwrap_or_default()
            .to_json()
            .into_bytes())
    }
}

/// The error of a re-encoded chunk that differs from its plan: the input changed between the
/// passes (the graph's read access must be held for both; a merge's inputs are immutable files
/// and the WAL delta held in memory).
fn changed() -> PersistError {
    PersistError::encode(
        "a snapshot",
        "the input changed while the snapshot was written (a chunk differs from its plan)",
    )
}

/// Where the elements of one kind go, one at a time in strictly increasing id order: the plan
/// or the emit pass of a merge checkpoint (`store::merge`), which produces its elements by
/// merging a base snapshot's segments with the WAL delta instead of walking a graph.
pub(crate) trait ElementSink {
    fn vertex(
        &mut self,
        id: &str,
        labels: &[CompactString],
        properties: &PropertyMap,
    ) -> Result<(), PersistError>;

    fn edge(
        &mut self,
        id: &str,
        label: Option<&str>,
        out_id: &str,
        in_id: &str,
        properties: Option<&PropertyMap>,
    ) -> Result<(), PersistError>;
}

/// The plan pass over a stream of elements of one kind (the push form of
/// [`SnapshotEncoder::plan_kind`]): every chunk encoded once, only its plan kept, segments
/// framed by the segment target. Segment files are numbered from `first_number` on.
pub(crate) struct KindPlanner {
    encoder: ChunkEncoder,
    builder: SegmentBuilder,
    position: usize,
    chunk_start: usize,
    plans: Vec<SegmentPlan>,
    /// Top-level properties of the planned elements.
    pub(crate) property_count: u64,
    /// The planned elements.
    pub(crate) element_count: u64,
    /// The largest number of bytes held for one chunk.
    pub(crate) peak_chunk_bytes: usize,
}

impl KindPlanner {
    pub(crate) fn new(
        kind: u8,
        graph_id: u128,
        options: &WriteOptions,
        first_number: usize,
    ) -> Self {
        let mut builder = SegmentBuilder::new(kind, graph_id, options);
        builder.number = first_number;
        Self {
            encoder: ChunkEncoder::new(kind, options.codec, options.chunk_bytes),
            builder,
            position: 0,
            chunk_start: 0,
            plans: Vec::new(),
            property_count: 0,
            element_count: 0,
            peak_chunk_bytes: 0,
        }
    }

    fn added(&mut self, closed: Option<EncodedChunk>, properties: u64) -> Result<(), PersistError> {
        self.position += 1;
        self.element_count += 1;
        self.property_count += properties;
        if let Some(chunk) = closed {
            self.closed(&chunk)?;
        }
        Ok(())
    }

    fn closed(&mut self, chunk: &EncodedChunk) -> Result<(), PersistError> {
        let held = self.encoder.held_bytes() + chunk.stored.capacity() + chunk.header.len();
        self.peak_chunk_bytes = self.peak_chunk_bytes.max(held);
        if let Some(done) = self.builder.push(self.chunk_start..self.position, chunk)? {
            self.plans.push(done);
        }
        self.chunk_start = self.position;
        Ok(())
    }

    /// Closes the last chunk and segment: the planned segments and the next free number.
    pub(crate) fn finish(mut self) -> Result<(Vec<SegmentPlan>, usize), PersistError> {
        if let Some(chunk) = self.encoder.finish() {
            self.closed(&chunk)?;
        }
        if let Some(done) = self.builder.close()? {
            self.plans.push(done);
        }
        Ok((self.plans, self.builder.number))
    }
}

impl ElementSink for KindPlanner {
    fn vertex(
        &mut self,
        id: &str,
        labels: &[CompactString],
        properties: &PropertyMap,
    ) -> Result<(), PersistError> {
        let closed = self.encoder.vertex(id, labels, properties)?;
        self.added(closed, properties.len() as u64)
    }

    fn edge(
        &mut self,
        id: &str,
        label: Option<&str>,
        out_id: &str,
        in_id: &str,
        properties: Option<&PropertyMap>,
    ) -> Result<(), PersistError> {
        let closed = self.encoder.edge(id, label, out_id, in_id, properties)?;
        self.added(closed, properties.map_or(0, |p| p.len() as u64))
    }
}

/// Where [`KindEmitter`] writes the planned segments: each one begun (a file created), its
/// bytes put, and ended (the file synced) in turn.
pub(crate) trait SegmentSink {
    fn begin(&mut self, plan: &SegmentPlan) -> Result<(), PersistError>;
    fn put(&mut self, bytes: &[u8]) -> Result<(), PersistError>;
    fn end(&mut self, plan: &SegmentPlan) -> Result<(), PersistError>;
}

/// The emit pass over the same stream of elements a [`KindPlanner`] planned: every chunk
/// encoded again, checked against its plan, and written into its planned segment.
pub(crate) struct KindEmitter<'p, S: SegmentSink> {
    plans: &'p [SegmentPlan],
    segment: usize,
    chunk: usize,
    open: bool,
    encoder: ChunkEncoder,
    sink: S,
}

impl<'p, S: SegmentSink> KindEmitter<'p, S> {
    pub(crate) fn new(kind: u8, options: &WriteOptions, plans: &'p [SegmentPlan], sink: S) -> Self {
        Self {
            plans,
            segment: 0,
            chunk: 0,
            open: false,
            encoder: ChunkEncoder::new(kind, options.codec, options.chunk_bytes),
            sink,
        }
    }

    fn write(&mut self, chunk: &EncodedChunk) -> Result<(), PersistError> {
        let plan = self.plans.get(self.segment).ok_or_else(changed)?;
        let chunk_plan = plan.chunks.get(self.chunk).ok_or_else(changed)?;
        if chunk.crc != chunk_plan.entry.crc || chunk.byte_len() != chunk_plan.byte_len {
            return Err(changed());
        }
        if !self.open {
            self.sink.begin(plan)?;
            self.sink.put(&plan.header)?;
            self.open = true;
        }
        self.sink.put(&chunk.header)?;
        self.sink.put(&chunk.stored)?;
        self.chunk += 1;
        if self.chunk == plan.chunks.len() {
            self.sink.put(&plan.footer)?;
            self.sink.end(plan)?;
            self.open = false;
            self.segment += 1;
            self.chunk = 0;
        }
        Ok(())
    }

    /// Closes the last chunk; every planned segment must be written by then.
    pub(crate) fn finish(mut self) -> Result<S, PersistError> {
        if let Some(chunk) = self.encoder.finish() {
            self.write(&chunk)?;
        }
        if self.segment != self.plans.len() {
            return Err(changed());
        }
        Ok(self.sink)
    }
}

impl<S: SegmentSink> ElementSink for KindEmitter<'_, S> {
    fn vertex(
        &mut self,
        id: &str,
        labels: &[CompactString],
        properties: &PropertyMap,
    ) -> Result<(), PersistError> {
        match self.encoder.vertex(id, labels, properties)? {
            Some(chunk) => self.write(&chunk),
            None => Ok(()),
        }
    }

    fn edge(
        &mut self,
        id: &str,
        label: Option<&str>,
        out_id: &str,
        in_id: &str,
        properties: Option<&PropertyMap>,
    ) -> Result<(), PersistError> {
        match self.encoder.edge(id, label, out_id, in_id, properties)? {
            Some(chunk) => self.write(&chunk),
            None => Ok(()),
        }
    }
}

/// Groups planned chunks into segments by the segment target (a segment closes after the
/// chunk that brings it to the target) and frames each.
struct SegmentBuilder {
    kind: u8,
    codec: super::codec::Codec,
    graph_id: u128,
    target: u64,
    /// The segment's length so far (header plus chunks).
    len: u64,
    /// The running CRC of the chunk bytes after the header.
    body_crc: u32,
    count: u64,
    chunks: Vec<ChunkPlan>,
    number: usize,
}

impl SegmentBuilder {
    fn new(kind: u8, graph_id: u128, options: &WriteOptions) -> Self {
        Self {
            kind,
            codec: options.codec,
            graph_id,
            target: options.segment_bytes.max(1),
            len: SEGMENT_HEADER_BYTES as u64,
            body_crc: 0,
            count: 0,
            chunks: Vec::new(),
            number: 0,
        }
    }

    fn push(
        &mut self,
        range: Range<usize>,
        chunk: &EncodedChunk,
    ) -> Result<Option<SegmentPlan>, PersistError> {
        self.body_crc = crc_append(crc_append(self.body_crc, &chunk.header), &chunk.stored);
        self.chunks.push(ChunkPlan {
            range,
            entry: ChunkIndexEntry {
                offset: self.len,
                first_id: chunk.first_id.clone(),
                last_id: chunk.last_id.clone(),
                crc: chunk.crc,
            },
            byte_len: chunk.byte_len(),
        });
        self.len += chunk.byte_len();
        self.count += u64::from(chunk.count);
        if self.len >= self.target {
            return self.close();
        }
        Ok(None)
    }

    /// Frames the current segment (none without chunks) and starts the next.
    fn close(&mut self) -> Result<Option<SegmentPlan>, PersistError> {
        if self.chunks.is_empty() {
            return Ok(None);
        }
        let chunks = std::mem::take(&mut self.chunks);
        let entries: Vec<ChunkIndexEntry> = chunks.iter().map(|c| c.entry.clone()).collect();
        let footer = segment_footer(&entries);
        let footer_offset = self.len;
        let header = segment_header(
            self.kind,
            self.codec,
            self.graph_id,
            chunks.len() as u32,
            footer_offset,
        );
        let rest_len = footer_offset - SEGMENT_HEADER_BYTES as u64 + footer.len() as u64;
        let byte_len = footer_offset + footer.len() as u64;
        if usize::try_from(byte_len).is_err() {
            return Err(PersistError::encode(
                "a snapshot",
                "a segment exceeds the address space of this platform; choose a smaller \
                 segment size",
            ));
        }
        let file_crc = crc_combine(crc(&header), crc_append(self.body_crc, &footer), rest_len);
        let prefix = if self.kind == KIND_EDGE { "e" } else { "v" };
        let entry = SegmentEntry {
            kind: self.kind,
            path: format!("{prefix}-{:06}.seg", self.number),
            first_id: entries
                .first()
                .map(|c| c.first_id.clone())
                .unwrap_or_default(),
            last_id: entries
                .last()
                .map(|c| c.last_id.clone())
                .unwrap_or_default(),
            count: std::mem::take(&mut self.count),
            byte_len,
            crc: file_crc,
        };
        self.number += 1;
        self.len = SEGMENT_HEADER_BYTES as u64;
        self.body_crc = 0;
        Ok(Some(SegmentPlan {
            entry,
            header,
            footer,
            chunks,
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use compact_str::CompactString;

    use super::super::codec::Codec;
    use super::super::snapshot::{WriteOptions, write_packed};
    use crate::graph::TraversalGraph;
    use crate::value::number::Number;
    use crate::value::{ElementProperty, PropertyMap};

    /// A deterministic generator (SplitMix64) for test graphs.
    struct Mix(u64);

    impl Mix {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    fn value(mix: &mut Mix, depth: u32) -> ElementProperty {
        match mix.below(if depth > 2 { 7 } else { 9 }) {
            0 => ElementProperty::Null,
            1 => ElementProperty::Boolean(mix.below(2) == 1),
            2 => ElementProperty::Number(Number::from_i64(mix.next() as i64)),
            3 => ElementProperty::Number(Number::from_f64(f64::from_bits(mix.next()))),
            4 => ElementProperty::String(
                format!(
                    "text {} ünï {}",
                    mix.next(),
                    "x".repeat(mix.below(80) as usize)
                )
                .into(),
            ),
            5 => ElementProperty::Uuid((u128::from(mix.next()) << 64) | u128::from(mix.next())),
            6 => ElementProperty::String(CompactString::default()),
            7 => ElementProperty::Array((0..mix.below(4)).map(|_| value(mix, depth + 1)).collect()),
            _ => {
                let mut object = PropertyMap::default();
                for k in 0..mix.below(4) {
                    object.insert(format!("k{k}"), value(mix, depth + 1));
                }
                ElementProperty::Object(Box::new(object))
            }
        }
    }

    /// A graph of `vertices` vertices and twice as many edges: ids in random order, multi-label
    /// and unlabeled vertices, unlabeled edges, every value type, nested values.
    pub(crate) fn generated_graph(vertices: usize, seed: u64) -> TraversalGraph {
        let mut mix = Mix(seed);
        let mut graph = TraversalGraph::new().with_graph_id(u128::from(seed) + 1);
        let labels = ["person", "software", "city", "ç-label"];
        let mut handles = Vec::with_capacity(vertices);
        for i in 0..vertices {
            let id = format!("{:x}-{i}", mix.next());
            let label_set: Vec<CompactString> = match mix.below(5) {
                0 => vec![],
                1 => vec![labels[0].into(), labels[3].into()],
                n => vec![labels[n as usize % labels.len()].into()],
            };
            let mut properties = PropertyMap::default();
            for k in 0..mix.below(6) {
                let key = format!("p{}", (k * 7 + mix.below(3)) % 11);
                properties.insert(key, value(&mut mix, 0));
            }
            let handle = graph
                .insert_vertex(Some(id.into()), label_set, properties)
                .expect("vertex");
            handles.push(handle);
        }
        if !handles.is_empty() {
            for i in 0..vertices * 2 {
                let from = handles[mix.below(handles.len() as u64) as usize];
                let to = handles[mix.below(handles.len() as u64) as usize];
                let label = match mix.below(4) {
                    0 => None,
                    n => Some(CompactString::from(
                        ["knows", "created", "lives"][n as usize - 1],
                    )),
                };
                let mut properties = PropertyMap::default();
                if mix.below(2) == 0 {
                    properties.insert("w", value(&mut mix, 0));
                }
                let id = format!("e{:x}-{i}", mix.next());
                graph
                    .insert_edge(Some(id.into()), label, from, to, properties)
                    .expect("edge");
            }
        }
        graph
    }

    pub(crate) fn graphs() -> Vec<(&'static str, TraversalGraph)> {
        vec![
            ("empty", TraversalGraph::new().with_graph_id(0xE)),
            (
                "modern",
                TraversalGraph::tinkerpop_modern().with_graph_id(0x40D),
            ),
            ("small", generated_graph(40, 1)),
            ("generated", generated_graph(1500, 7)),
        ]
    }

    pub(crate) fn codecs() -> Vec<Codec> {
        vec![
            Codec::None,
            Codec::Lz4,
            #[cfg(feature = "persist-zstd")]
            Codec::Zstd,
        ]
    }

    /// Chunk and segment sizes: one element per chunk and segment, several chunks per
    /// segment, the defaults.
    pub(crate) const SIZES: [(u32, u64); 4] =
        [(64, 1), (700, 5000), (4096, 1 << 20), (1 << 20, 256 << 20)];

    /// A fingerprint of a packed snapshot's segments: every segment row (with the CRC of the
    /// whole file) and every chunk index row. It covers every byte the encoder produced and
    /// nothing that varies between runs or builds (creation time, the memory estimate).
    pub(crate) fn fingerprint(packed: &[u8]) -> u32 {
        use super::super::codec::{crc, put_str, put_u8, put_u32, put_u64};
        let manifest = super::super::snapshot::PackedReader::open(packed)
            .unwrap()
            .manifest;
        let mut out = Vec::new();
        for segment in &manifest.segments {
            put_u8(&mut out, segment.kind);
            put_str(&mut out, &segment.path);
            put_str(&mut out, &segment.first_id);
            put_str(&mut out, &segment.last_id);
            put_u64(&mut out, segment.count);
            put_u64(&mut out, segment.byte_len);
            put_u32(&mut out, segment.crc);
        }
        for row in &manifest.chunks {
            put_u32(&mut out, row.segment);
            put_u64(&mut out, row.chunk.offset);
            put_str(&mut out, &row.chunk.first_id);
            put_str(&mut out, &row.chunk.last_id);
            put_u32(&mut out, row.chunk.crc);
        }
        crc(&out)
    }

    /// The fingerprints of the in-memory encoder this one replaced (recorded from it with
    /// [`Codec::None`], whose bytes depend on this crate alone; a compression library's
    /// version may change compressed bytes legitimately). The streaming encoder was proven
    /// byte-identical to it for every codec before it was removed.
    const GOLDEN: [(&str, u32, u64, u32); 16] = [
        ("empty", 64, 1, 0x00000000),
        ("empty", 700, 5000, 0x00000000),
        ("empty", 4096, 1048576, 0x00000000),
        ("empty", 1048576, 268435456, 0x00000000),
        ("modern", 64, 1, 0xa74b3da5),
        ("modern", 700, 5000, 0x06555241),
        ("modern", 4096, 1048576, 0x06555241),
        ("modern", 1048576, 268435456, 0x06555241),
        ("small", 64, 1, 0x4630a26b),
        ("small", 700, 5000, 0xe48c73a2),
        ("small", 4096, 1048576, 0xbe91347a),
        ("small", 1048576, 268435456, 0x31c48ae4),
        ("generated", 64, 1, 0x87909ff4),
        ("generated", 700, 5000, 0x5020781e),
        ("generated", 4096, 1048576, 0xbb4633da),
        ("generated", 1048576, 268435456, 0x545b24d1),
    ];

    /// The streaming encoder writes the segments the in-memory encoder it replaced wrote.
    #[test]
    fn the_streaming_encoder_writes_the_segments_of_the_in_memory_one() {
        let graphs = graphs();
        for (name, chunk, segment, expected) in GOLDEN {
            let graph = &graphs.iter().find(|(n, _)| *n == name).unwrap().1;
            let options = WriteOptions::new()
                .with_codec(Codec::None)
                .with_chunk_bytes(chunk)
                .with_segment_bytes(segment);
            let mut packed = Vec::new();
            write_packed(graph, &mut packed, &options).unwrap();
            assert_eq!(
                fingerprint(&packed),
                expected,
                "{name} chunk {chunk} segment {segment}"
            );
        }
    }

    /// Every codec, chunk and segment size: the same bytes on every write (the two passes are
    /// deterministic), and the snapshot loads back to the same graph.
    #[test]
    fn every_codec_and_size_writes_the_same_bytes_and_loads_back() {
        use super::super::snapshot::{ReadOptions, read_snapshot};
        for (name, graph) in graphs() {
            for codec in codecs() {
                for (chunk, segment) in SIZES {
                    let options = WriteOptions::new()
                        .with_codec(codec)
                        .with_chunk_bytes(chunk)
                        .with_segment_bytes(segment)
                        .with_created_at(1_700_000_000_000_000);
                    let mut first = Vec::new();
                    write_packed(&graph, &mut first, &options).unwrap();
                    let mut second = Vec::new();
                    write_packed(&graph, &mut second, &options).unwrap();
                    assert!(first == second, "{name} {codec:?} {chunk} {segment}");
                    let loaded = read_snapshot(first.as_slice(), &ReadOptions::new()).unwrap();
                    assert_eq!(loaded.vertex_count(), graph.vertex_count());
                    assert_eq!(loaded.edge_count(), graph.edge_count());
                }
            }
        }
    }
}
