//! The Store's WAL segment files (format spec section 9) and its `marks` file (section 10): the
//! journal's writer with segment rotation, the bookkeeping the auto checkpoint needs, and the
//! readers of both files.

use std::io::{Read, Write};

use super::super::dir::StoreFile;
use super::files::DirPath;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::super::codec::{Reader, crc, put_u32};
use super::super::error::PersistError;
use super::super::journal::JournalFiles;
use super::super::wal::{JournalReader, JournalRecord, WAL_HEADER_BYTES, WalHeader, encode_mark};
use super::files::{GraphFile, MARKS_FILE, WAL_DIR, sync_dir, wal_name, write_graph_file};
use crate::graph::hooks::Mark;

/// What the journal's files share with the Store (lock-free flags and counters, and the wake-up
/// of the checkpoint thread).
#[derive(Debug, Default)]
pub(crate) struct WalShared {
    /// Set by `close`: every later record is refused (the commit is vetoed).
    pub(crate) closed: AtomicBool,
    /// Set when damage was found on disk while the store is open and its damage policy is
    /// `maintenance`: every later record (commit, mark, checkpoint record) is refused; the
    /// refusal is `(found_by, problems)` (format spec 13.3).
    pub(crate) frozen: Mutex<Option<(String, usize)>>,
    /// Set by a checkpoint: the next record starts a new segment.
    pub(crate) rotate: AtomicBool,
    /// WAL bytes written since the last snapshot (the auto checkpoint's measure).
    pub(crate) since_checkpoint: AtomicU64,
    /// The auto checkpoint threshold in bytes (`0`: off).
    pub(crate) checkpoint_after: AtomicU64,
    /// The blob ledger lacks frames (an append failed and the file was removed): every user of
    /// the ledger rebuilds its frames from the WAL until the next open rewrites the file.
    pub(crate) ledger_stale: AtomicBool,
    /// Serializes the ledger's appends (commits) with its rewrite and the blob collection of a
    /// prune, an attic removal or a rollback `delete`: a frame is either in the file the
    /// collection reads or its commit's blob is still pending.
    pub(crate) ledger_lock: Mutex<()>,
    /// The checkpoint thread's wake-up: `true` = a checkpoint is due or the store closes.
    pub(crate) wake: Mutex<bool>,
    pub(crate) wake_signal: Condvar,
}

impl WalShared {
    /// The refusal of a store frozen by damage found while it is open (`None`: not frozen).
    pub(crate) fn frozen(&self) -> Option<(String, usize)> {
        self.frozen.lock().ok().and_then(|f| f.clone())
    }

    pub(crate) fn wake(&self) {
        if let Ok(mut due) = self.wake.lock() {
            *due = true;
            self.wake_signal.notify_all();
        }
    }
}

/// The journal's writer: the current WAL segment file.
pub(crate) struct WalWriter {
    pub(crate) file: Box<dyn StoreFile>,
}

impl Write for WalWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// The Store side of the journal: rotation of WAL segments and the `marks` file.
pub(crate) struct StoreWalFiles {
    pub(crate) dir: DirPath,
    pub(crate) graph_id: u128,
    pub(crate) segment_bytes: u64,
    /// The current segment's first commit sequence number (its name).
    pub(crate) current: u64,
    pub(crate) shared: Arc<WalShared>,
    /// A segment could not be started cleanly and its remains could not be removed: refuse
    /// everything until the store is reopened.
    pub(crate) broken: bool,
    /// The store's `GRAPH` file: a refused record that cannot be cut off is recorded there.
    pub(crate) graph_file: Arc<Mutex<GraphFile>>,
    /// Injected failures (crate tests only).
    pub(crate) faults: Option<Arc<Faults>>,
}

/// Failures injected into a Store's WAL (crate tests: a failing `fsync`, a failing cut).
#[derive(Debug, Default)]
pub(crate) struct Faults {
    pub(crate) fail_sync: AtomicBool,
    pub(crate) fail_truncate: AtomicBool,
}

impl Faults {
    pub(crate) fn sync(&self) -> std::io::Result<()> {
        if self.fail_sync.load(Ordering::Acquire) {
            return Err(std::io::Error::other("injected fsync failure"));
        }
        Ok(())
    }
}

/// Creates WAL segment `first` with its header, synced, and syncs the WAL directory.
pub(crate) fn create_segment(
    dir: &DirPath,
    graph_id: u128,
    first: u64,
) -> std::io::Result<Box<dyn StoreFile>> {
    let path = dir.join(WAL_DIR).join(wal_name(first));
    let header = WalHeader {
        graph_id,
        first_commit_seq: first,
        created_at: crate::graph::undo::now_micros(),
    };
    let created = path.create(true).and_then(|mut file| {
        file.write_all(&header.encode())?;
        file.sync()?;
        Ok(file)
    });
    match created {
        Ok(file) => {
            sync_dir(&dir.join(WAL_DIR)).map_err(|err| std::io::Error::other(err.to_string()))?;
            Ok(file)
        }
        Err(err) => {
            if err.kind() != std::io::ErrorKind::AlreadyExists {
                let _ = path.remove_file();
            }
            Err(err)
        }
    }
}

/// Records in `GRAPH` that segment `first` was started (format spec 9.8), after the segment
/// was created and synced: from now on a missing or emptied segment `first` is damage. A
/// failed write only weakens the check (the older head stays; never a false alarm).
pub(crate) fn record_wal_head(dir: &DirPath, graph_file: &Mutex<GraphFile>, first: u64) {
    let Ok(mut file) = graph_file.lock() else {
        return;
    };
    if file.wal_head == Some(first) {
        return;
    }
    file.wal_head = Some(first);
    if let Err(err) = write_graph_file(dir, &file) {
        tracing::warn!("recording the new WAL segment {first} in GRAPH failed: {err}");
    }
}

/// Opens segment `path` to append at `offset` (after a torn tail was cut).
pub(crate) fn open_segment_at(path: &DirPath, offset: u64) -> std::io::Result<Box<dyn StoreFile>> {
    path.open_write(offset)
}

impl JournalFiles<WalWriter> for StoreWalFiles {
    fn before_record(
        &mut self,
        writer: &mut WalWriter,
        offset: u64,
        next_seq: u64,
    ) -> std::io::Result<Option<(u64, u64)>> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(std::io::Error::other("the store is closed"));
        }
        if let Some((found_by, problems)) = self.shared.frozen() {
            return Err(std::io::Error::other(format!(
                "the store is read-only: {found_by} found damage on disk ({problems} problem(s)) \
                 and its damage policy is \"maintenance\""
            )));
        }
        if self.broken {
            return Err(std::io::Error::other(
                "an earlier WAL segment could not be started; reopen the store",
            ));
        }
        let wanted = offset > self.segment_bytes || self.shared.rotate.load(Ordering::Acquire);
        if !wanted || self.current == next_seq {
            return Ok(None);
        }
        // Everything of the closing segment is made durable first (also under the Interval
        // and Os durabilities), so only the last segment can ever end in a torn record.
        writer.file.sync()?;
        let file = match create_segment(&self.dir, self.graph_id, next_seq) {
            Ok(file) => file,
            Err(err) => {
                let path = self.dir.join(WAL_DIR).join(wal_name(next_seq));
                if path.exists() && err.kind() != std::io::ErrorKind::AlreadyExists {
                    self.broken = path.remove_file().is_err();
                }
                return Err(err);
            }
        };
        writer.file = file;
        self.current = next_seq;
        self.shared.rotate.store(false, Ordering::Release);
        record_wal_head(&self.dir, &self.graph_file, next_seq);
        Ok(Some((next_seq, WAL_HEADER_BYTES as u64)))
    }

    fn record_written(&mut self, bytes: u64) {
        let total = self
            .shared
            .since_checkpoint
            .fetch_add(bytes, Ordering::AcqRel)
            + bytes;
        // Every record while the level is at or above the threshold wakes the checkpoint thread,
        // not only the one that crosses it: a record written during a running checkpoint (which
        // subtracts only what it covers) or after a failed one would otherwise leave the level
        // above the threshold with nobody ever waking the thread again. A wake while a
        // checkpoint runs keeps the thread due; it re-checks the level when it gets there.
        let limit = self.shared.checkpoint_after.load(Ordering::Acquire);
        if limit > 0 && total >= limit {
            self.shared.wake();
        }
    }

    fn discard(
        &mut self,
        writer: &mut WalWriter,
        segment: u64,
        offset: u64,
    ) -> std::io::Result<()> {
        let injected = self
            .faults
            .as_ref()
            .is_some_and(|f| f.fail_truncate.load(Ordering::Acquire));
        let cut = if injected {
            Err(std::io::Error::other("injected truncate failure"))
        } else {
            // A panicking backend counts as a failed cut, so the cut is still recorded below.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.file.set_len(offset)))
                .unwrap_or_else(|payload| {
                    Err(std::io::Error::other(format!(
                        "the cut panicked: {}",
                        super::super::journal::panic_text(payload.as_ref())
                    )))
                })
        };
        let Err(err) = cut else {
            return Ok(());
        };
        // The refused record may still be in the file: record where the valid WAL ends, so
        // neither the next open nor a read-only load ever replays it (format spec 9.7).
        let mut file = self
            .graph_file
            .lock()
            .map_err(|_| std::io::Error::other("a store lock is poisoned"))?;
        file.wal_cut = Some((segment, offset));
        if let Err(write) = write_graph_file(&self.dir, &file) {
            tracing::error!(
                "the refused WAL record at segment {segment}, offset {offset} could neither be \
                 cut off ({err}) nor recorded ({write}); do not reopen the store before checking it"
            );
        }
        Err(err)
    }

    fn commit_written(&mut self, changes: &crate::graph::change::ChangeSet) {
        if self.shared.ledger_stale.load(Ordering::Acquire) {
            return;
        }
        let frames = super::blobs::commit_frames(changes, self.graph_id);
        if frames.is_empty() {
            return;
        }
        let _ledger = self
            .shared
            .ledger_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The ledger is derived (rebuilt from the WAL at the next open): a failed append
        // removes it and marks it stale, so no collection trusts an incomplete file.
        if super::blobs::append_frames(&self.dir, &frames).is_err() {
            self.shared.ledger_stale.store(true, Ordering::Release);
            let _ = self.dir.join(super::blobs::LEDGER_FILE).remove_file();
        }
    }

    fn mark_written(&mut self, mark: &Mark) {
        // The marks file is derived (rebuilt from the WAL when it is missing): a failed append
        // removes it, so the next open rebuilds it instead of listing an incomplete file.
        if append_mark(&self.dir, mark).is_err() {
            let _ = self.dir.join(MARKS_FILE).remove_file();
        }
    }
}

/// One frame of the `marks` file: `length u32 | CRC u32 | Mark payload`.
fn mark_frame(mark: &Mark) -> Vec<u8> {
    let payload = encode_mark(mark);
    let mut out = Vec::with_capacity(payload.len() + 8);
    put_u32(&mut out, payload.len() as u32);
    put_u32(&mut out, crc(&payload));
    out.extend_from_slice(&payload);
    out
}

/// Appends `mark` to the `marks` file and syncs it.
pub(crate) fn append_mark(dir: &DirPath, mark: &Mark) -> std::io::Result<()> {
    dir.join(MARKS_FILE).append(&mark_frame(mark))
}

/// Rewrites the `marks` file with `marks` (atomically).
pub(crate) fn write_marks(dir: &DirPath, marks: &[Mark]) -> Result<(), PersistError> {
    super::files::write_atomic(dir, MARKS_FILE, &marks_bytes(marks))
}

/// The bytes of a `marks` file holding `marks`.
pub(crate) fn marks_bytes(marks: &[Mark]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for mark in marks {
        bytes.extend_from_slice(&mark_frame(mark));
    }
    bytes
}

/// Reads the `marks` file: `Ok(None)` when it is missing or damaged (rebuild it from the WAL).
pub(crate) fn read_marks(dir: &DirPath) -> Result<Option<Vec<Mark>>, PersistError> {
    let path = dir.join(MARKS_FILE);
    let bytes = match super::files::read_bounded(&path, 256 << 20) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };
    Ok(decode_marks(&bytes))
}

/// The marks of `marks` a target can reach in the store (or backup) in `dir`: those at or after
/// its oldest snapshot (format spec 10). The file keeps the others (a prune does not rewrite it,
/// and a backup with older snapshots still reaches them), so listings filter here.
pub(crate) fn reachable_marks(
    dir: &DirPath,
    mut marks: Vec<Mark>,
) -> Result<Vec<Mark>, PersistError> {
    if let Some((oldest, _)) = super::files::list_snapshots(dir)?.first() {
        marks.retain(|mark| mark.commit_seq() >= *oldest);
    }
    Ok(marks)
}

/// Decodes the frames of a `marks` file; `None` on any damage.
pub(crate) fn decode_marks(bytes: &[u8]) -> Option<Vec<Mark>> {
    let mut marks = Vec::new();
    let mut r = Reader::new(bytes);
    while !r.is_empty() {
        let length = r.u32().ok()? as usize;
        let stored = r.u32().ok()?;
        let payload = r.bytes(length).ok()?;
        if crc(payload) != stored {
            return None;
        }
        let mut p = Reader::new(payload);
        let commit_seq = p.u64().ok()?;
        let time = p.i64().ok()?;
        let name = p.str().ok()?.to_string();
        if !p.is_empty() {
            return None;
        }
        marks.push(Mark::new(name, commit_seq, time));
    }
    Some(marks)
}

/// Opens a WAL segment file for reading, at most `limit` bytes of it (a durable end).
pub(crate) fn open_segment_reader(
    path: &DirPath,
    limit: Option<u64>,
) -> Result<Box<dyn Read + Send>, PersistError> {
    let reader = path
        .open_read()
        .map_err(|err| PersistError::io("opening a WAL segment", err))?;
    Ok(match limit {
        Some(limit) => Box::new(reader.take(limit)),
        None => Box::new(reader),
    })
}

/// Every mark in the given WAL segments, in order (rebuilding the `marks` file). Stops quietly
/// at a torn tail or at damage (the marks before it are kept; `verify` reports the damage).
pub(crate) fn scan_marks(segments: &[(u64, DirPath)], max_depth: usize) -> Vec<Mark> {
    let mut marks = Vec::new();
    for (index, (_, path)) in segments.iter().enumerate() {
        let Ok(source) = open_segment_reader(path, None) else {
            continue;
        };
        let Ok(mut reader) = JournalReader::open(source, index, max_depth) else {
            continue;
        };
        while let Ok(Some((_, _, record))) = reader.next_known_record(&mut |_| {}) {
            if let JournalRecord::Mark(mark) = record {
                marks.push(mark);
            }
        }
    }
    marks
}

/// What is wrong with the SET of WAL segments (format spec 9.8), as `(file name, reason)`: a
/// segment whose header names another first commit than its file name (another segment's
/// bytes), a segment GRAPH records as started (`wal_head`) or an older one that is missing or
/// shorter than its header (emptied). A segment newer than the recorded one and shorter than
/// its header is a crash while it was being started: not a problem. `segments` are the store's
/// (without the ones after a recorded WAL cut), in order.
pub(crate) fn wal_set_problems(
    segments: &[(u64, DirPath)],
    file: &GraphFile,
) -> Vec<(String, String)> {
    let mut problems = Vec::new();
    let head = file.wal_head;
    for (first, path) in segments {
        let name = format!("{WAL_DIR}/{}", wal_name(*first));
        let len = path.len().unwrap_or(0);
        if len < WAL_HEADER_BYTES as u64 {
            if head.is_some_and(|head| *first <= head) {
                problems.push((
                    name,
                    format!(
                        "the segment is shorter than its header ({len} bytes), but it was \
                         started and made durable before (GRAPH records segment {} as the \
                         newest): its records are lost",
                        head.unwrap_or(0)
                    ),
                ));
            }
            continue;
        }
        let Ok(bytes) = path.read_range(0, WAL_HEADER_BYTES) else {
            continue;
        };
        if let Ok(header) = WalHeader::decode(&bytes)
            && header.first_commit_seq != *first
        {
            problems.push((
                name,
                format!(
                    "its header names first commit {}, its file name {first}: the file holds \
                     another segment's bytes, its own records are lost",
                    header.first_commit_seq
                ),
            ));
        }
    }
    if let Some(head) = head
        && !segments.iter().any(|(first, _)| *first == head)
    {
        problems.push((
            format!("{WAL_DIR}/{}", wal_name(head)),
            format!(
                "the newest WAL segment GRAPH records (first commit {head}) is missing: the \
                 commits from {head} on are lost"
            ),
        ));
    }
    problems
}
