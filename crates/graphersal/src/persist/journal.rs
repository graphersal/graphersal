//! [`Journal`]: the write-ahead log of a graph over any `Write` (format spec section 9).

use std::collections::HashSet;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::codec::Codec;
use super::error::PersistError;
use super::wal::{
    EncodedCommit, PART_BODY_BYTES, RECORD_CHECKPOINT, RECORD_COMMIT, RECORD_COMMIT_PART,
    RECORD_MARK, WAL_HEADER_BYTES, WalHeader, encode_commit_body, encode_mark, frame_record,
};
use crate::graph::PersistentStorage;
use crate::graph::TraversalGraph;
use crate::graph::change::ChangeSet;
use crate::graph::hooks::{CommitHook, HookError, JournalHook, JournalSlot, Mark};

/// When the journal makes a written record durable (calls its sync function, `fsync` for a
/// file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Durability {
    /// After every commit and mark, before the commit becomes final (the default): a commit
    /// that returned `Ok` survives a crash of the machine.
    #[default]
    EveryCommit,
    /// At a commit when at least this much time passed since the last sync: a crash may lose
    /// the commits of the last interval (never a commit in the middle).
    Interval(Duration),
    /// Never explicitly: the record is handed to the operating system (`flush`) and written
    /// when it decides. A crash of the process loses nothing, a crash of the machine may.
    Os,
}

/// The durable end of a journal: the byte offset just after the last record whose sync
/// completed (with [`Durability::Os`]: whose flush completed), that record's commit sequence
/// number, and the segment it is in. Lock-free: a backup reads it without any graph lock and
/// copies the journal only up to [`DurableEnd::offset`].
#[derive(Debug, Clone, Default)]
pub struct DurableEnd(Arc<DurableInner>);

#[derive(Debug, Default)]
struct DurableInner {
    /// Even while stable, odd while [`DurableEnd::advance`] writes (a sequence lock: the one
    /// writer is the journal, readers retry instead of waiting).
    version: AtomicU64,
    segment: AtomicU64,
    offset: AtomicU64,
    commit_seq: AtomicU64,
}

/// A consistent reading of a [`DurableEnd`]: the three values belong together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DurablePosition {
    /// The segment the durable end is in: the `first commit_seq` of its header (a Store's WAL
    /// file name; a stream journal has one segment).
    pub segment: u64,
    /// The byte offset in that segment after the last durable record.
    pub offset: u64,
    /// The commit sequence number of the last durable record.
    pub commit_seq: u64,
}

impl DurableEnd {
    /// The byte offset in the journal stream after the last durable record (`0` before the
    /// header is durable).
    pub fn offset(&self) -> u64 {
        self.position().offset
    }

    /// The commit sequence number of the last durable record.
    pub fn commit_seq(&self) -> u64 {
        self.position().commit_seq
    }

    /// Segment, offset and commit sequence number, read consistently without a lock.
    pub fn position(&self) -> DurablePosition {
        let inner = &*self.0;
        loop {
            let before = inner.version.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let position = DurablePosition {
                segment: inner.segment.load(Ordering::Relaxed),
                offset: inner.offset.load(Ordering::Relaxed),
                commit_seq: inner.commit_seq.load(Ordering::Relaxed),
            };
            std::sync::atomic::fence(Ordering::Acquire);
            if inner.version.load(Ordering::Relaxed) == before {
                return position;
            }
        }
    }

    fn advance(&self, segment: u64, offset: u64, commit_seq: u64) {
        let inner = &*self.0;
        let version = inner.version.load(Ordering::Relaxed);
        inner
            .version
            .store(version.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        inner.segment.store(segment, Ordering::Relaxed);
        inner.offset.store(offset, Ordering::Relaxed);
        inner.commit_seq.store(commit_seq, Ordering::Relaxed);
        inner
            .version
            .store(version.wrapping_add(2), Ordering::Release);
    }
}

/// The Store's part of a journal (crate-internal): WAL segment rotation and the `marks` file.
pub(crate) trait JournalFiles<W>: Send + Sync {
    /// Called before a record that may contain commit `next_seq` is written at `offset` of the
    /// current segment. May switch `writer` to a new segment (header written and durable) and
    /// then returns `(segment, offset)` of the new one. An error vetoes the record (nothing of
    /// it was written).
    fn before_record(
        &mut self,
        writer: &mut W,
        offset: u64,
        next_seq: u64,
    ) -> std::io::Result<Option<(u64, u64)>>;

    /// After a record of `bytes` bytes was written (and synced per the durability).
    fn record_written(&mut self, bytes: u64);

    /// After a mark record is durable.
    fn mark_written(&mut self, mark: &Mark);

    /// A record at `offset` of segment `segment` was refused (its write or sync failed): cut
    /// the segment back to `offset` and make that durable, so the refused record is never
    /// replayed. When even that fails, record the cut elsewhere (the Store's `GRAPH` file)
    /// and return the error.
    fn discard(&mut self, writer: &mut W, segment: u64, offset: u64) -> std::io::Result<()>;
}

/// An in-memory journal target that can be shared and read back (tests, the browser, a
/// journal shipped elsewhere): every clone writes to and reads from the same bytes.
#[derive(Debug, Clone, Default)]
pub struct SharedBuffer(Arc<parking_lot::Mutex<Vec<u8>>>);

impl SharedBuffer {
    /// An empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of everything written so far.
    pub fn contents(&self) -> Vec<u8> {
        self.0.lock().clone()
    }

    /// The number of bytes written so far.
    pub fn len(&self) -> usize {
        self.0.lock().len()
    }

    /// Whether nothing was written yet.
    pub fn is_empty(&self) -> bool {
        self.0.lock().is_empty()
    }
}

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type SyncFn<W> = Box<dyn FnMut(&mut W) -> std::io::Result<()> + Send + Sync>;
type TruncateFn<W> = Box<dyn FnMut(&mut W, u64) -> std::io::Result<()> + Send + Sync>;

/// The write-ahead log of a graph: a [commit hook](CommitHook) that writes one record per
/// committed unit (a lossless [`ChangeSet`]) and one per mark to a `Write`, before the commit
/// becomes final.
///
/// - **Write-ahead:** the record is written (and synced per [`Durability`]) in
///   `before_commit`; a failed write or sync vetoes the commit, which rolls back, so memory and
///   journal never diverge and no caller gets "ok" for a lost commit.
/// - **Always last:** the journal has its own slot after every user hook, whatever the
///   registration order, so no hook can veto a commit after its record is written. The commit
///   sequence number is assigned (and checked) before; nothing fallible follows the record.
/// - **Poisoning:** after a failed write or sync the stream's state is unknown, so the journal
///   rejects every later commit and mark ([`PersistError::Poisoned`]); recover the graph and
///   attach a new journal. Before it poisons itself, a journal that can truncate its target
///   ([`Journal::for_file`], [`Journal::with_truncate`]) cuts the refused record off again and
///   syncs, so a commit the caller was told failed is never replayed by a later recovery.
/// - **Panics:** a panic in the journal's own write path (an engine bug, or a panicking
///   writer, sync or `StoreDir`) is treated like a failed write: the partial or complete
///   record is cut off again (as above), the journal poisons itself, and then the panic
///   continues. The graph rolls the unit back on the way out (its commit catches the panic),
///   so neither memory nor the journal keeps the unit; every later commit and mark is refused
///   with [`PersistError::Poisoned`].
/// - **One per graph**, attached with [`Journal::attach`], which writes the stream header
///   (lineage id, first commit sequence number). Write a snapshot at the same position (before
///   or after attaching, before the next commit) so a recovery has a base.
pub struct Journal<W: Write + Send + Sync + 'static> {
    writer: W,
    durability: Durability,
    sync: SyncFn<W>,
    truncate: Option<TruncateFn<W>>,
    codec: Codec,
    last_sync: Option<web_time::Instant>,
    poisoned: Option<String>,
    marks: HashSet<String>,
    durable: DurableEnd,
    offset: u64,
    /// The current segment (the `first commit_seq` of its header).
    segment: u64,
    /// The last commit sequence number written (or the graph's at attach).
    last_seq: u64,
    record: Vec<u8>,
    /// The payload of the Commit part being written (reused per part).
    part: Vec<u8>,
    /// The uncompressed body bytes per Commit part (format spec 9.3.1).
    part_bytes: usize,
    /// Where the record being written starts in the current segment, from its first byte
    /// until it is written and synced: what a panic in the write path must cut off again.
    pending: Option<u64>,
    files: Option<Box<dyn JournalFiles<W>>>,
}

impl<W: Write + Send + Sync + 'static> std::fmt::Debug for Journal<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("durability", &self.durability)
            .field("offset", &self.offset)
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl<W: Write + Send + Sync + 'static> Journal<W> {
    /// A journal writing to `writer`. Its sync function is `flush`; give a file's `fsync` with
    /// [`Self::with_sync`] (or use [`Journal::for_file`]).
    pub fn new(writer: W, durability: Durability) -> Self {
        Self {
            writer,
            durability,
            sync: Box::new(|writer: &mut W| writer.flush()),
            truncate: None,
            codec: Codec::Lz4,
            last_sync: None,
            poisoned: None,
            marks: HashSet::new(),
            durable: DurableEnd::default(),
            offset: 0,
            segment: 0,
            last_seq: 0,
            record: Vec::new(),
            part: Vec::new(),
            part_bytes: PART_BODY_BYTES,
            pending: None,
            files: None,
        }
    }

    /// The Store's segment rotation and marks file.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn with_files(mut self, files: Box<dyn JournalFiles<W>>) -> Self {
        self.files = Some(files);
        self
    }

    /// The function that makes written bytes durable (called per [`Durability`]).
    pub fn with_sync(
        mut self,
        sync: impl FnMut(&mut W) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.sync = Box::new(sync);
        self
    }

    /// The function that cuts the journal back to a length in bytes of this journal's stream
    /// (counted from where [`Journal::attach`] wrote the header) and makes the cut durable. It
    /// runs when a record's write or sync failed: the refused record is cut off before the
    /// journal poisons itself, so a later recovery never replays a commit that was refused.
    /// [`Journal::for_file`] sets one (`set_len` + `fsync`).
    pub fn with_truncate(
        mut self,
        truncate: impl FnMut(&mut W, u64) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.truncate = Some(Box::new(truncate));
        self
    }

    /// Mark names already used in this lineage (from [`super::RecoveryReport::marks`]): a new
    /// mark with one of them is rejected.
    pub fn with_known_marks<I: IntoIterator<Item = S>, S: Into<String>>(
        mut self,
        names: I,
    ) -> Self {
        self.marks.extend(names.into_iter().map(Into::into));
        self
    }

    /// The compression of large records (more than 4 KiB): [`Codec::Lz4`] by default.
    pub fn with_codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// The uncompressed body bytes per Commit part (default 64 MiB, at least 1, at most
    /// 1 GiB): a commit body above it is written in parts (format spec 9.3.1). A reader
    /// accepts any part size, so this is a test knob, not a format parameter.
    #[doc(hidden)]
    pub fn with_commit_part_bytes(mut self, bytes: usize) -> Self {
        self.part_bytes = bytes.clamp(1, super::codec::MAX_BLOCK_BYTES);
        self
    }

    /// Publishes this journal's durable end through `durable` (a Store keeps one durable end
    /// across the journals it attaches, e.g. after an in-place rollback).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn with_durable_end(mut self, durable: DurableEnd) -> Self {
        self.durable = durable;
        self
    }

    /// The journal's [`DurableEnd`], readable without any lock while the journal runs.
    pub fn durable_end(&self) -> DurableEnd {
        self.durable.clone()
    }

    /// Writes the journal header (the graph's lineage id and the next commit sequence number)
    /// and registers the journal on `graph`. Fails when `graph` already has a journal or a unit
    /// is open, or when the header cannot be written. Returns the [`DurableEnd`].
    pub fn attach(mut self, graph: &mut TraversalGraph) -> Result<DurableEnd, PersistError> {
        let first_commit_seq = Self::check_attach(graph)?;
        let header = WalHeader {
            graph_id: graph.graph_id(),
            first_commit_seq,
            created_at: crate::graph::undo::now_micros(),
        };
        self.writer
            .write_all(&header.encode())
            .and_then(|()| self.writer.flush())
            .and_then(|()| (self.sync)(&mut self.writer))
            .map_err(|err| PersistError::io("writing the journal header", err))?;
        self.register(graph, first_commit_seq, WAL_HEADER_BYTES as u64)
    }

    /// Prepares the journal to continue a segment whose header and records are already in the
    /// writer (a Store reopening its last WAL segment, or starting one): nothing is written, the
    /// next record goes to `offset` of segment `segment`, the last commit written is
    /// `last_commit_seq` (the graph's). The Store keeps the journal in its own journal cell,
    /// which sits in the graph's journal slot (`store::graph`), so a reload in place replaces
    /// the journal without touching the slot.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn continuing(
        mut self,
        last_commit_seq: u64,
        segment: u64,
        offset: u64,
    ) -> (Self, DurableEnd) {
        let durable = self.prime(last_commit_seq, segment, offset);
        (self, durable)
    }

    /// The checks of an attach; returns the next commit sequence number.
    fn check_attach(graph: &TraversalGraph) -> Result<u64, PersistError> {
        if graph.undo.hooks.has_journal() {
            return Err(crate::error::GraphError::Unsupported {
                operation: "a second journal on one graph",
            }
            .into());
        }
        if graph.undo_depth() > 0 {
            return Err(crate::error::GraphError::Unsupported {
                operation: "attaching a journal while a unit is open",
            }
            .into());
        }
        Ok(graph.last_commit_seq().checked_add(1).ok_or(
            crate::error::GraphError::SequenceOverflow {
                sequence: crate::error::COMMIT_SEQUENCE,
            },
        )?)
    }

    fn register(
        mut self,
        graph: &mut TraversalGraph,
        segment: u64,
        offset: u64,
    ) -> Result<DurableEnd, PersistError> {
        let durable = self.prime(graph.last_commit_seq(), segment, offset);
        graph.attach_journal(JournalSlot::new(Box::new(self)))?;
        Ok(durable)
    }

    /// Sets the write position and the last commit written; publishes it as the durable end.
    fn prime(&mut self, last_commit_seq: u64, segment: u64, offset: u64) -> DurableEnd {
        self.offset = offset;
        self.segment = segment;
        self.last_seq = last_commit_seq;
        self.last_sync = Some(web_time::Instant::now());
        self.durable.advance(segment, offset, self.last_seq);
        self.durable.clone()
    }

    /// A record written from `start` was refused (its write or sync failed): cut it off again
    /// and sync (format spec 9.7), then poison the journal. When the cut is durable, the
    /// durable end moves to `start` (every earlier record is synced with it).
    fn refuse(&mut self, start: u64, reason: String) -> HookError {
        self.pending = None;
        // The cut runs the writer's own code: a panic in it counts as a failed cut.
        let segment = self.segment;
        let cut = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(files) = &mut self.files {
                Some(files.discard(&mut self.writer, segment, start))
            } else {
                self.truncate
                    .as_mut()
                    .map(|truncate| truncate(&mut self.writer, start))
            }
        }))
        .unwrap_or_else(|payload| {
            Some(Err(std::io::Error::other(format!(
                "the cut panicked: {}",
                panic_text(payload.as_ref())
            ))))
        });
        let reason = match cut {
            Some(Ok(())) => {
                self.offset = start;
                self.durable.advance(self.segment, start, self.last_seq);
                format!("{reason} (the refused record was cut off again)")
            }
            Some(Err(err)) => format!(
                "{reason}; cutting the refused record off failed too ({err}), its position is \
                 recorded so a recovery never replays it"
            ),
            None => reason,
        };
        self.poison(reason)
    }

    fn poison(&mut self, reason: String) -> HookError {
        self.poisoned = Some(reason.clone());
        Box::new(PersistError::Poisoned { reason })
    }

    /// Writes the framed record in `self.record` and makes it durable per the durability.
    /// `next_seq` is the first commit sequence number the record's segment must allow (the
    /// commit's own, or the next one for a mark or a checkpoint).
    fn append(&mut self, commit_seq: u64, next_seq: u64) -> Result<(), HookError> {
        let start = self.begin_record(next_seq)?;
        self.write_record(start)?;
        self.finish_record(start, commit_seq)
    }

    /// Writes a commit in parts (format spec 9.3.1): rotation once before part 0, the parts in
    /// order through one reused buffer, ONE sync after the last part. A failed write or sync
    /// of any part cuts the segment back to the start of part 0 (9.7) and poisons the journal;
    /// the durable end moves only after the last part.
    fn append_parts(
        &mut self,
        encoded: &EncodedCommit,
        parts: u32,
        commit_seq: u64,
    ) -> Result<(), HookError> {
        let start = self.begin_record(commit_seq)?;
        for part in 0..parts {
            encoded.put_part(part, parts, self.part_bytes, &mut self.part);
            self.record.clear();
            if let Err(err) =
                frame_record(&mut self.record, RECORD_COMMIT_PART, commit_seq, &self.part)
            {
                return Err(self.refuse(start, err.to_string()));
            }
            self.write_record(start)?;
        }
        let written = self.finish_record(start, commit_seq);
        // A commit in parts is large: its buffers are not kept for the next commit.
        self.record = Vec::new();
        self.part = Vec::new();
        written
    }

    /// The checks and the segment rotation before a record (or the parts of one commit);
    /// returns where it starts.
    fn begin_record(&mut self, next_seq: u64) -> Result<u64, HookError> {
        if let Some(reason) = &self.poisoned {
            return Err(Box::new(PersistError::Poisoned {
                reason: reason.clone(),
            }));
        }
        if let Some(files) = &mut self.files {
            match files.before_record(&mut self.writer, self.offset, next_seq) {
                Ok(None) => {}
                Ok(Some((segment, offset))) => {
                    self.segment = segment;
                    self.offset = offset;
                    self.last_sync = Some(web_time::Instant::now());
                    self.durable.advance(segment, offset, self.last_seq);
                }
                Err(err) => {
                    return Err(Box::new(PersistError::io(
                        "starting the next journal segment",
                        err,
                    )));
                }
            }
        }
        let start = self.offset;
        self.pending = Some(start);
        Ok(start)
    }

    /// Writes the framed bytes in `self.record` after what was written so far; a failure
    /// refuses everything from `start` on.
    fn write_record(&mut self, start: u64) -> Result<(), HookError> {
        if let Err(err) = self
            .writer
            .write_all(&self.record)
            .and_then(|()| self.writer.flush())
        {
            let reason = format!("writing a journal record failed: {err}");
            return Err(self.refuse(start, reason));
        }
        self.offset += self.record.len() as u64;
        Ok(())
    }

    /// Makes everything from `start` durable per the durability and publishes the durable end.
    fn finish_record(&mut self, start: u64, commit_seq: u64) -> Result<(), HookError> {
        // The durable end never moves back: a Checkpoint record names the (older) snapshot
        // position while later commits may already be in the WAL before it.
        let durable_seq = commit_seq.max(self.last_seq);
        let sync_now = match self.durability {
            Durability::EveryCommit => true,
            Durability::Interval(every) => {
                self.last_sync.is_none_or(|last| last.elapsed() >= every)
            }
            Durability::Os => false,
        };
        if sync_now {
            if let Err(err) = (self.sync)(&mut self.writer) {
                self.offset = start;
                let reason = format!("syncing the journal failed: {err}");
                return Err(self.refuse(start, reason));
            }
            self.last_sync = Some(web_time::Instant::now());
            self.durable.advance(self.segment, self.offset, durable_seq);
        } else if self.durability == Durability::Os {
            self.durable.advance(self.segment, self.offset, durable_seq);
        }
        self.pending = None;
        if let Some(files) = &mut self.files {
            files.record_written(self.offset - start);
        }
        Ok(())
    }

    /// Runs `write` (encoding and writing one record, or a sync) so that a panic in it is
    /// handled like a failed write: the record started is cut off again (format spec 9.7, a
    /// cut that fails is recorded by the Store), the journal poisons itself, and the panic
    /// continues. The graph's commit catches it on the way out and rolls the unit back, so
    /// neither memory nor the journal keeps the unit.
    fn guarded<T>(
        &mut self,
        what: &str,
        write: impl FnOnce(&mut Self) -> Result<T, HookError>,
    ) -> Result<T, HookError> {
        self.pending = None;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write(&mut *self))) {
            Ok(result) => result,
            Err(payload) => {
                let reason = format!("a panic while {what}: {}", panic_text(payload.as_ref()));
                tracing::error!("the journal is poisoned by {reason}");
                match self.pending.take() {
                    Some(start) => drop(self.refuse(start, reason)),
                    None => drop(self.poison(reason)),
                }
                std::panic::resume_unwind(payload)
            }
        }
    }

    /// Syncs everything written so far (a Store closing) and advances the durable end.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn sync_all(&mut self) -> Result<(), HookError> {
        if let Some(reason) = &self.poisoned {
            return Err(Box::new(PersistError::Poisoned {
                reason: reason.clone(),
            }));
        }
        if let Err(err) = self
            .writer
            .flush()
            .and_then(|()| (self.sync)(&mut self.writer))
        {
            return Err(self.poison(format!("syncing the journal failed: {err}")));
        }
        self.last_sync = Some(web_time::Instant::now());
        self.durable
            .advance(self.segment, self.offset, self.last_seq);
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Journal<std::fs::File> {
    /// A journal writing to `file` whose sync is `File::sync_data` (`fsync`).
    ///
    /// A refused record (a failed write or sync) is cut off again with `set_len` and `fsync`
    /// before the journal poisons itself; lengths count from the file's length at this call,
    /// so a journal appended to an existing file cuts only its own bytes.
    pub fn for_file(file: std::fs::File, durability: Durability) -> Self {
        use std::io::{Seek, SeekFrom};
        let base = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        Journal::new(file, durability)
            .with_sync(|file: &mut std::fs::File| file.sync_data())
            .with_truncate(move |file: &mut std::fs::File, len| {
                file.set_len(base + len)?;
                file.seek(SeekFrom::Start(base + len))?;
                file.sync_all()
            })
    }
}

impl<W: Write + Send + Sync + 'static> CommitHook for Journal<W> {
    fn name(&self) -> &str {
        "journal"
    }

    fn before_commit(&mut self, changes: &ChangeSet) -> Result<(), HookError> {
        if let Some(reason) = &self.poisoned {
            return Err(Box::new(PersistError::Poisoned {
                reason: reason.clone(),
            }));
        }
        self.guarded("writing a commit record", |journal| {
            let encoded = encode_commit_body(changes, journal.codec)
                .map_err(|err| Box::new(err) as HookError)?;
            let parts = encoded
                .parts(journal.part_bytes)
                .map_err(|err| Box::new(err) as HookError)?;
            if parts > 1 {
                // Format spec 9.3.1: a body above the part size is written in parts.
                journal.append_parts(&encoded, parts, changes.commit_seq())?;
                journal.last_seq = changes.commit_seq();
                return Ok(());
            }
            let payload = encoded.single_payload();
            drop(encoded);
            journal.record.clear();
            frame_record(
                &mut journal.record,
                RECORD_COMMIT,
                changes.commit_seq(),
                &payload,
            )
            .map_err(|err| Box::new(err) as HookError)?;
            journal.append(changes.commit_seq(), changes.commit_seq())?;
            journal.last_seq = changes.commit_seq();
            Ok(())
        })
    }
}

impl<W: Write + Send + Sync + 'static> JournalHook for Journal<W> {
    fn record_mark(&mut self, mark: &Mark) -> Result<(), HookError> {
        if self.marks.contains(mark.name()) {
            return Err(
                format!("the mark '{}' already exists in this journal", mark.name()).into(),
            );
        }
        self.guarded("writing a mark record", |journal| {
            journal.record.clear();
            frame_record(
                &mut journal.record,
                RECORD_MARK,
                mark.commit_seq(),
                &encode_mark(mark),
            )
            .map_err(|err| Box::new(err) as HookError)?;
            journal.append(mark.commit_seq(), journal.last_seq.saturating_add(1))?;
            journal.marks.insert(mark.name().to_string());
            if let Some(files) = &mut journal.files {
                files.mark_written(mark);
            }
            Ok(())
        })
    }

    fn record_checkpoint(&mut self, commit_seq: u64, time: i64) -> Result<(), HookError> {
        self.guarded("writing a checkpoint record", |journal| {
            let mut payload = Vec::with_capacity(16);
            payload.extend_from_slice(&commit_seq.to_le_bytes());
            payload.extend_from_slice(&time.to_le_bytes());
            journal.record.clear();
            frame_record(&mut journal.record, RECORD_CHECKPOINT, commit_seq, &payload)
                .map_err(|err| Box::new(err) as HookError)?;
            journal.append(commit_seq, journal.last_seq.saturating_add(1))
        })
    }

    fn sync_now(&mut self) -> Result<(), HookError> {
        self.guarded("syncing the journal", |journal| journal.sync_all())
    }
}

/// The message of a panic payload (`&str` or `String`), for the poisoned journal's reason.
pub(crate) fn panic_text(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(no message)")
}
