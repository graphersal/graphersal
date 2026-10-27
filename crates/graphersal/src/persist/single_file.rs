//! The single-file store (format spec section 14, native only): a whole Store in ONE file, as a
//! [`StoreDir`] backend ([`SingleFileDir`]).
//!
//! The file is a log-structured container: after a fixed preamble and two header slots, every
//! change of a name is APPENDED as a framed record (an append to a file, a whole-file
//! replacement, a create, a truncation, a directory, a rename, a removal), and from time to
//! time the whole name -> extents table is appended as a record too. The header slots (a
//! generation number and a CRC-32C each, written alternately) point at the latest table; an
//! open loads it and replays the records after it. Nothing inside the file is ever overwritten
//! (only the two header slots are rewritten in place): a torn update leaves the previous
//! table and every earlier record intact, and space that a removal frees is reused only by
//! [`StoreDir::compact`], which copies the live data into a new file and replaces the old one
//! atomically.
//!
//! - **Durability.** Every record carries the offset up to which the file was known durable
//!   (`fsync`ed) when it was written. Bytes that cannot be read, followed only by records that
//!   were never made durable after them, are the torn tail of a crash (cut at the next write);
//!   followed by a record written after they were made durable, they are damage
//!   ([`StoreDir::check`], the Store opens in maintenance mode).
//! - **Data passes through.** A data record whose payload checksum fails is still handed to the
//!   Store (its own checksums locate the damage, as on a directory); the container's checksum
//!   decides only between a torn tail and damage.
//! - **Locks.** The writer lock is the OS lock (`flock` / `LockFileEx`) on the file itself, so
//!   one process writes; every [`SingleFileDir`] of one path in a process shares one in-memory
//!   container. A process without the writer lock re-reads what another process appended
//!   before it answers; a backup pin freezes its view (the file is append-only, so a frozen
//!   view stays readable).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};

use super::PersistError;
use super::codec::{Reader, crc, crc_append, put_str, put_u32, put_u64, put_uvar};
use super::dir::{
    DirCheck, DirCompaction, DirEntry, DirLockGuard, EntryKind, LockKind, SpaceUsage, StoreDir,
    StoreFile, check_name,
};

/// The file extension of a single-file store (`graph.gstore`).
pub const SINGLE_FILE_EXTENSION: &str = "gstore";

/// Magic of the preamble (bytes 0..8, written once).
const PREAMBLE_MAGIC: &[u8; 8] = b"GRSLFILE";
/// The container version.
const CONTAINER_VERSION: u32 = 1;
/// Magic of a header slot.
const HEAD_MAGIC: &[u8; 8] = b"GRSLHEAD";
/// The two header slots (64 bytes each, in different 4 KiB pages).
const SLOT_AT: [u64; 2] = [4096, 8192];
const SLOT_BYTES: usize = 64;
/// Where the first record starts.
const DATA_START: u64 = 12288;
/// The sync marker of a record frame.
const FRAME_MARKER: &[u8; 4] = b"GSFR";
/// The fixed part of a frame header (before the type fields).
const FRAME_FIXED: usize = 36;
/// The smallest frame: fixed part, no fields, both CRCs, no payload.
const FRAME_MIN: u64 = 44;
/// The largest type-field section of a frame.
const MAX_FIELDS: usize = 16 << 10;

const KIND_APPEND: u8 = 1;
const KIND_CREATE: u8 = 2;
const KIND_PUT: u8 = 3;
const KIND_TRUNCATE: u8 = 4;
const KIND_MKDIR: u8 = 5;
const KIND_RENAME: u8 = 6;
const KIND_REMOVE: u8 = 7;
const KIND_TABLE: u8 = 8;

/// A write of a [`StoreFile`] is buffered up to this size before it becomes a record.
const PENDING_BYTES: usize = 1 << 20;
/// Compaction copies a file in records of at most this size.
const COPY_CHUNK: u64 = 64 << 20;
/// A new table is appended once the records since the last one reach
/// `max(TABLE_MIN_TAIL, TABLE_TAIL_FACTOR * table size)`.
const TABLE_MIN_TAIL: u64 = 16 << 20;
const TABLE_TAIL_FACTOR: u64 = 16;
/// Reads go through windows of this size (many small extents are read with one call).
const WINDOW: usize = 1 << 20;

/// Every opened file gets a new epoch (a late `fsync` result never updates a newer file).
static EPOCH: AtomicU64 = AtomicU64::new(1);

/// The containers open in this process, by absolute path: every [`SingleFileDir`] of one file
/// shares one.
fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<Container>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Container>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn invalid(reason: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.into())
}

fn not_found(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{name}: no such file"))
}

fn parent_of(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Whether `path` names a single-file store ([`store_dir_at`](super::store_dir_at)): an
/// existing regular file, or a missing path with the extension `.gstore`.
pub(crate) fn names_single_file(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) => meta.is_file(),
        Err(_) => path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(SINGLE_FILE_EXTENSION)),
    }
}

// ---------------------------------------------------------------------------------------------
// Positioned I/O
// ---------------------------------------------------------------------------------------------

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], at: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, at)
}

#[cfg(unix)]
fn write_at(file: &File, buf: &[u8], at: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, at)
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut at: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, at) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                at += n as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_at(file: &File, mut buf: &[u8], mut at: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, at) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                buf = &buf[n..];
                at += n as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_at(file: &File, buf: &mut [u8], at: u64) -> io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(buf)
}

#[cfg(not(any(unix, windows)))]
fn write_at(file: &File, buf: &[u8], at: u64) -> io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(at))?;
    file.write_all(buf)
}

/// The identity of an open file (device and inode on Unix): a compaction by another process
/// replaces the file under the same path.
#[cfg(unix)]
fn identity(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn identity(_: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// Syncs the directory holding `path` (a new or replaced file name).
fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// The free space of the file system holding `path`'s directory (`Ok(None)`: not known on
/// this platform); a failing `statvfs` is the error.
#[cfg(unix)]
fn free_space_at(path: &Path) -> io::Result<Option<u64>> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let stat = rustix::fs::statvfs(parent).map_err(io::Error::from)?;
    Ok(Some(stat.f_bavail.saturating_mul(stat.f_frsize)))
}

#[cfg(not(unix))]
fn free_space_at(_: &Path) -> io::Result<Option<u64>> {
    Ok(None)
}

// ---------------------------------------------------------------------------------------------
// The name tree
// ---------------------------------------------------------------------------------------------

/// A run of a file's bytes in the container: logical `start..start+len` of the file are the
/// container bytes `at..at+len`; `at == 0` is a hole (zeros: bytes a damaged record lost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extent {
    start: u64,
    at: u64,
    len: u64,
}

#[derive(Debug, Clone, Default)]
struct Entry {
    len: u64,
    extents: Vec<Extent>,
}

impl Entry {
    fn push(&mut self, at: u64, len: u64) {
        if len == 0 {
            return;
        }
        self.extents.push(Extent {
            start: self.len,
            at,
            len,
        });
        self.len += len;
    }

    /// Cuts to `len` (at most the length); returns the data bytes dropped.
    fn truncate(&mut self, len: u64) -> u64 {
        let mut dropped = 0;
        while let Some(last) = self.extents.last_mut() {
            if last.start >= len {
                if last.at != 0 {
                    dropped += last.len;
                }
                self.extents.pop();
            } else {
                if last.start + last.len > len {
                    let keep = len - last.start;
                    if last.at != 0 {
                        dropped += last.len - keep;
                    }
                    last.len = keep;
                }
                break;
            }
        }
        self.len = self.len.min(len);
        dropped
    }

    fn data_bytes(&self) -> u64 {
        self.extents
            .iter()
            .filter(|e| e.at != 0)
            .map(|e| e.len)
            .sum()
    }

    /// The extents covering logical `offset..offset+len`, clipped to it.
    fn slice(&self, offset: u64, len: u64) -> Vec<Extent> {
        let end = offset.saturating_add(len);
        let first = self.extents.partition_point(|e| e.start + e.len <= offset);
        let mut out = Vec::new();
        for e in &self.extents[first..] {
            if e.start >= end {
                break;
            }
            let from = e.start.max(offset);
            let to = (e.start + e.len).min(end);
            out.push(Extent {
                start: from,
                at: if e.at == 0 {
                    0
                } else {
                    e.at + (from - e.start)
                },
                len: to - from,
            });
        }
        out
    }
}

/// The names of the container: directories and files with their extents.
#[derive(Debug, Clone, Default)]
struct Tree {
    dirs: BTreeSet<String>,
    files: BTreeMap<String, Entry>,
    /// The container bytes the files' extents hold.
    data_bytes: u64,
}

/// One change of names, as a record says it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Append { name: String, offset: u64 },
    Create { name: String },
    Put { name: String },
    Truncate { name: String, len: u64 },
    Mkdir { name: String },
    Rename { from: String, to: String },
    Remove { name: String },
    Table,
}

impl Op {
    fn kind(&self) -> u8 {
        match self {
            Op::Append { .. } => KIND_APPEND,
            Op::Create { .. } => KIND_CREATE,
            Op::Put { .. } => KIND_PUT,
            Op::Truncate { .. } => KIND_TRUNCATE,
            Op::Mkdir { .. } => KIND_MKDIR,
            Op::Rename { .. } => KIND_RENAME,
            Op::Remove { .. } => KIND_REMOVE,
            Op::Table => KIND_TABLE,
        }
    }

    fn fields(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Op::Append { name, offset } => {
                put_str(&mut out, name);
                put_uvar(&mut out, *offset);
            }
            Op::Truncate { name, len } => {
                put_str(&mut out, name);
                put_uvar(&mut out, *len);
            }
            Op::Create { name } | Op::Put { name } | Op::Mkdir { name } | Op::Remove { name } => {
                put_str(&mut out, name);
            }
            Op::Rename { from, to } => {
                put_str(&mut out, from);
                put_str(&mut out, to);
            }
            Op::Table => {}
        }
        out
    }

    fn decode(kind: u8, fields: &[u8]) -> Option<Op> {
        let mut r = Reader::new(fields);
        let name = |r: &mut Reader<'_>| -> Option<String> {
            let name = r.str().ok()?;
            (!name.is_empty() && check_name(name).is_ok()).then(|| name.to_string())
        };
        let op = match kind {
            KIND_APPEND => Op::Append {
                name: name(&mut r)?,
                offset: r.uvar().ok()?,
            },
            KIND_CREATE => Op::Create {
                name: name(&mut r)?,
            },
            KIND_PUT => Op::Put {
                name: name(&mut r)?,
            },
            KIND_TRUNCATE => Op::Truncate {
                name: name(&mut r)?,
                len: r.uvar().ok()?,
            },
            KIND_MKDIR => Op::Mkdir {
                name: name(&mut r)?,
            },
            KIND_RENAME => Op::Rename {
                from: name(&mut r)?,
                to: name(&mut r)?,
            },
            KIND_REMOVE => Op::Remove {
                name: name(&mut r)?,
            },
            KIND_TABLE => Op::Table,
            _ => return None,
        };
        r.is_empty().then_some(op)
    }
}

impl Tree {
    fn is_dir(&self, name: &str) -> bool {
        name.is_empty() || self.dirs.contains(name)
    }

    fn check_parent(&self, name: &str) -> io::Result<()> {
        if self.is_dir(parent_of(name)) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{name}: the directory does not exist"),
            ))
        }
    }

    fn below(&self, dir: &str) -> (Vec<String>, Vec<String>) {
        let prefix = format!("{dir}/");
        (
            self.files
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect(),
            self.dirs
                .iter()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect(),
        )
    }

    fn make_parents(&mut self, name: &str) {
        let mut dir = parent_of(name);
        while !dir.is_empty() {
            self.dirs.insert(dir.to_string());
            dir = parent_of(dir);
        }
    }

    fn remove_file_entry(&mut self, name: &str) -> Option<Entry> {
        let entry = self.files.remove(name)?;
        self.data_bytes -= entry.data_bytes();
        Some(entry)
    }

    fn set_file(&mut self, name: &str, entry: Entry) {
        self.data_bytes += entry.data_bytes();
        if let Some(old) = self.files.insert(name.to_string(), entry) {
            self.data_bytes -= old.data_bytes();
        }
    }

    /// The live checks of an operation (what a file system refuses), before it is logged.
    fn check(&self, op: &Op) -> io::Result<()> {
        match op {
            Op::Append { name, .. } => {
                if self.dirs.contains(name) {
                    return Err(io::Error::other(format!("{name} is a directory")));
                }
                self.check_parent(name)
            }
            Op::Create { name } | Op::Put { name } => {
                if self.dirs.contains(name) {
                    return Err(io::Error::other(format!("{name} is a directory")));
                }
                self.check_parent(name)
            }
            Op::Truncate { name, .. } => self.files.get(name).map(|_| ()).ok_or(not_found(name)),
            Op::Mkdir { name } => {
                let mut dir = name.as_str();
                while !dir.is_empty() {
                    if self.files.contains_key(dir) {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("{dir} is a file"),
                        ));
                    }
                    dir = parent_of(dir);
                }
                Ok(())
            }
            Op::Rename { from, to } => {
                self.check_parent(to)?;
                if self.files.contains_key(from) {
                    if self.dirs.contains(to) {
                        return Err(io::Error::other(format!("{to} is a directory")));
                    }
                    return Ok(());
                }
                if !self.dirs.contains(from) {
                    return Err(not_found(from));
                }
                if self.dirs.contains(to) {
                    return Ok(());
                }
                if self.files.contains_key(to) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{to} exists"),
                    ));
                }
                if to.starts_with(&format!("{from}/")) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{to} is inside {from}"),
                    ));
                }
                Ok(())
            }
            Op::Remove { .. } | Op::Table => Ok(()),
        }
    }

    /// Applies a logged operation; its payload is the container bytes `at..at+len`. Lenient
    /// (a replay after damage may meet anything); returns a note when bytes are missing.
    fn apply(&mut self, op: &Op, at: u64, len: u64) -> Option<String> {
        match op {
            Op::Append { name, offset } => {
                if self.dirs.contains(name) {
                    return Some(format!("an append to {name}, which is a directory"));
                }
                self.make_parents(name);
                let mut entry = self.remove_file_entry(name).unwrap_or_default();
                let mut note = None;
                if entry.len > *offset {
                    entry.truncate(*offset);
                } else if entry.len < *offset {
                    note = Some(format!(
                        "{name}: bytes {}..{offset} are missing (a lost record)",
                        entry.len
                    ));
                    let gap = offset - entry.len;
                    entry.push(0, gap);
                }
                entry.push(at, len);
                self.set_file(name, entry);
                note
            }
            Op::Create { name } => {
                if !self.dirs.contains(name) {
                    self.make_parents(name);
                    self.set_file(name, Entry::default());
                }
                None
            }
            Op::Put { name } => {
                if !self.dirs.contains(name) {
                    self.make_parents(name);
                    let mut entry = Entry::default();
                    entry.push(at, len);
                    self.set_file(name, entry);
                }
                None
            }
            Op::Truncate { name, len } => {
                if let Some(entry) = self.files.get_mut(name) {
                    let dropped = entry.truncate(*len);
                    self.data_bytes -= dropped;
                }
                None
            }
            Op::Mkdir { name } => {
                if !self.files.contains_key(name) {
                    self.make_parents(name);
                    self.dirs.insert(name.clone());
                }
                None
            }
            Op::Rename { from, to } => {
                if let Some(entry) = self.remove_file_entry(from) {
                    self.make_parents(to);
                    self.set_file(to, entry);
                    return None;
                }
                if !self.dirs.contains(from) {
                    return None;
                }
                let (files, dirs) = self.below(from);
                if self.dirs.contains(to) {
                    // The completed move of a copying backend: only the source goes.
                    for name in files {
                        self.remove_file_entry(&name);
                    }
                    for name in dirs {
                        self.dirs.remove(&name);
                    }
                    self.dirs.remove(from);
                    return None;
                }
                if self.files.contains_key(to) || to.starts_with(&format!("{from}/")) {
                    return None;
                }
                let moved = |name: &str| format!("{to}{}", &name[from.len()..]);
                for name in files {
                    if let Some(entry) = self.files.remove(&name) {
                        self.files.insert(moved(&name), entry);
                    }
                }
                for name in dirs {
                    self.dirs.remove(&name);
                    self.dirs.insert(moved(&name));
                }
                self.dirs.remove(from);
                self.make_parents(to);
                self.dirs.insert(to.clone());
                None
            }
            Op::Remove { name } => {
                if self.remove_file_entry(name).is_none() && self.dirs.remove(name) {
                    let (files, dirs) = self.below(name);
                    for file in files {
                        self.remove_file_entry(&file);
                    }
                    for dir in dirs {
                        self.dirs.remove(&dir);
                    }
                }
                None
            }
            Op::Table => None,
        }
    }

    /// The table record's payload.
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_uvar(&mut out, self.dirs.len() as u64);
        for dir in &self.dirs {
            put_str(&mut out, dir);
        }
        put_uvar(&mut out, self.files.len() as u64);
        for (name, entry) in &self.files {
            put_str(&mut out, name);
            put_uvar(&mut out, entry.len);
            put_uvar(&mut out, entry.extents.len() as u64);
            for e in &entry.extents {
                put_uvar(&mut out, e.at);
                put_uvar(&mut out, e.len);
            }
        }
        out
    }

    /// Decodes a table; every extent must lie in `DATA_START..limit` (or be a hole).
    fn decode(bytes: &[u8], limit: u64) -> Result<Tree, String> {
        let mut r = Reader::new(bytes);
        let err = |e: super::codec::DecodeError| e.reason;
        let mut tree = Tree::default();
        let dirs = r.count(1).map_err(err)?;
        for _ in 0..dirs {
            let name = r.str().map_err(err)?;
            if name.is_empty() || check_name(name).is_err() {
                return Err(format!(
                    "a directory name {name:?} that is not a store name"
                ));
            }
            tree.dirs.insert(name.to_string());
        }
        let files = r.count(3).map_err(err)?;
        for _ in 0..files {
            let name = r.str().map_err(err)?.to_string();
            if name.is_empty() || check_name(&name).is_err() || tree.dirs.contains(&name) {
                return Err(format!("a file name {name:?} that is not a store name"));
            }
            let len = r.uvar().map_err(err)?;
            let count = r.count(2).map_err(err)?;
            let mut entry = Entry::default();
            for _ in 0..count {
                let at = r.uvar().map_err(err)?;
                let n = r.uvar().map_err(err)?;
                let inside = at == 0
                    || (at >= DATA_START && at.checked_add(n).is_some_and(|end| end <= limit));
                if !inside || n == 0 {
                    return Err(format!("{name}: an extent outside the container's data"));
                }
                entry.push(at, n);
            }
            if entry.len != len {
                return Err(format!("{name}: the extents do not add up to its length"));
            }
            if tree.files.contains_key(&name) {
                return Err(format!("{name} is listed twice"));
            }
            tree.set_file(&name, entry);
        }
        if !r.is_empty() {
            return Err("bytes after the table".into());
        }
        Ok(tree)
    }

    /// The size a compaction would write (the preamble, the slots, one directory record per
    /// directory, the files' data in records of at most [`COPY_CHUNK`], the table).
    fn compacted_size(&self) -> u64 {
        let mut size = DATA_START;
        let mut table = 2 * 10 + FRAME_MIN;
        for dir in &self.dirs {
            size += FRAME_MIN + 3 + dir.len() as u64;
            table += 3 + dir.len() as u64;
        }
        for (name, entry) in &self.files {
            let chunks = entry.len.div_ceil(COPY_CHUNK).max(1);
            size += chunks * (FRAME_MIN + 3 + name.len() as u64 + 10) + entry.len;
            table += 3 + name.len() as u64 + 20 + chunks * 15;
        }
        size + table
    }
}

// ---------------------------------------------------------------------------------------------
// Frames and headers
// ---------------------------------------------------------------------------------------------

/// A decoded frame header.
#[derive(Debug, Clone)]
struct Frame {
    kind: u8,
    seq: u64,
    synced: u64,
    op: Op,
    payload_len: u64,
    payload_crc: u32,
    head_len: u64,
    total: u64,
}

/// What is at a position of the file.
enum Head {
    Frame(Frame),
    /// Fewer bytes than a frame header (a torn last write).
    Short,
    /// A valid header whose payload runs past the end (a torn last write).
    PastEnd,
    /// Not a valid frame header.
    Bad,
}

/// Encodes a frame header (everything before the payload).
fn encode_head(seq: u64, synced: u64, op: &Op, payload_len: u64, payload_crc: u32) -> Vec<u8> {
    let fields = op.fields();
    let mut out = Vec::with_capacity(FRAME_FIXED + fields.len() + 8);
    out.extend_from_slice(FRAME_MARKER);
    out.push(op.kind());
    out.extend_from_slice(&[0, 0, 0]);
    put_u64(&mut out, seq);
    put_u64(&mut out, synced);
    put_u32(&mut out, fields.len() as u32);
    put_u64(&mut out, payload_len);
    out.extend_from_slice(&fields);
    let head_crc = crc(&out);
    put_u32(&mut out, head_crc);
    put_u32(&mut out, payload_crc);
    out
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(b)
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}

/// Reads the frame header at `pos` of a file of `limit` bytes.
fn read_head(file: &File, pos: u64, limit: u64) -> io::Result<Head> {
    if limit.saturating_sub(pos) < FRAME_MIN {
        return Ok(Head::Short);
    }
    let mut fixed = [0u8; FRAME_FIXED];
    read_at(file, &mut fixed, pos)?;
    if &fixed[..4] != FRAME_MARKER || fixed[5..8] != [0, 0, 0] || !(1..=8).contains(&fixed[4]) {
        return Ok(Head::Bad);
    }
    let fields_len = u32_at(&fixed, 24) as usize;
    if fields_len > MAX_FIELDS {
        return Ok(Head::Bad);
    }
    let head_len = (FRAME_FIXED + fields_len + 8) as u64;
    if limit - pos < head_len {
        return Ok(Head::Short);
    }
    let mut rest = vec![0u8; fields_len + 8];
    read_at(file, &mut rest, pos + FRAME_FIXED as u64)?;
    let head_crc = crc_append(crc(&fixed), &rest[..fields_len]);
    if head_crc != u32_at(&rest, fields_len) {
        return Ok(Head::Bad);
    }
    let kind = fixed[4];
    let synced = u64_at(&fixed, 16);
    let payload_len = u64_at(&fixed, 28);
    let Some(op) = Op::decode(kind, &rest[..fields_len]) else {
        return Ok(Head::Bad);
    };
    let Some(total) = head_len.checked_add(payload_len) else {
        return Ok(Head::Bad);
    };
    // A record is written after the durable end it names, never before.
    if synced > pos
        || (kind != KIND_APPEND && kind != KIND_PUT && kind != KIND_TABLE && payload_len != 0)
    {
        return Ok(Head::Bad);
    }
    let frame = Frame {
        kind,
        seq: u64_at(&fixed, 8),
        synced,
        op,
        payload_len,
        payload_crc: u32_at(&rest, fields_len + 4),
        head_len,
        total,
    };
    if pos.checked_add(total).is_none_or(|end| end > limit) {
        return Ok(Head::PastEnd);
    }
    Ok(Head::Frame(frame))
}

/// The CRC-32C of `len` bytes from `at`, read in windows.
fn crc_of(file: &File, at: u64, len: u64) -> io::Result<u32> {
    let mut value = 0u32;
    let mut buf = vec![0u8; WINDOW.min(usize::try_from(len).unwrap_or(WINDOW))];
    let mut done = 0;
    while done < len {
        let n = (len - done).min(buf.len() as u64) as usize;
        read_at(file, &mut buf[..n], at + done)?;
        value = crc_append(value, &buf[..n]);
        done += n as u64;
    }
    Ok(value)
}

/// Whether the bytes `from..limit` are all zero (space a crash left unwritten).
fn all_zero(file: &File, from: u64, limit: u64) -> io::Result<bool> {
    let mut buf = vec![0u8; WINDOW];
    let mut at = from;
    while at < limit {
        let n = (limit - at).min(WINDOW as u64) as usize;
        read_at(file, &mut buf[..n], at)?;
        if buf[..n].iter().any(|b| *b != 0) {
            return Ok(false);
        }
        at += n as u64;
    }
    Ok(true)
}

/// The next position at or after `from` that holds a complete valid frame header.
fn resync(file: &File, from: u64, limit: u64) -> io::Result<Option<u64>> {
    let mut buf = vec![0u8; WINDOW];
    let mut at = from;
    while at < limit {
        let n = (limit - at).min(WINDOW as u64) as usize;
        read_at(file, &mut buf[..n], at)?;
        for i in 0..n {
            if buf[i] == FRAME_MARKER[0]
                && buf.get(i..i + 4).is_none_or(|w| w == FRAME_MARKER)
                && let Head::Frame(_) = read_head(file, at + i as u64, limit)?
            {
                return Ok(Some(at + i as u64));
            }
        }
        if n < 4 {
            break;
        }
        at += (n - 3) as u64;
    }
    Ok(None)
}

/// The last valid frame from `from` on (structure only): its offset and durable end.
fn last_frame(file: &File, from: u64, limit: u64) -> io::Result<Option<(u64, u64)>> {
    let mut pos = from;
    let mut last = None;
    while pos < limit {
        match read_head(file, pos, limit)? {
            Head::Frame(frame) => {
                last = Some((pos, frame.synced));
                pos += frame.total;
            }
            Head::Bad => match resync(file, pos + 1, limit)? {
                Some(next) => pos = next,
                None => break,
            },
            Head::Short | Head::PastEnd => break,
        }
    }
    Ok(last)
}

/// A header slot: the latest table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    generation: u64,
    table_at: u64,
    table_len: u64,
    table_seq: u64,
}

impl Header {
    fn encode(&self) -> [u8; SLOT_BYTES] {
        let mut out = Vec::with_capacity(SLOT_BYTES);
        out.extend_from_slice(HEAD_MAGIC);
        put_u64(&mut out, self.generation);
        put_u64(&mut out, self.table_at);
        put_u64(&mut out, self.table_len);
        put_u64(&mut out, self.table_seq);
        out.resize(SLOT_BYTES - 4, 0);
        let check = crc(&out);
        put_u32(&mut out, check);
        let mut slot = [0u8; SLOT_BYTES];
        slot.copy_from_slice(&out);
        slot
    }

    /// `Ok(None)`: an unused (all-zero) slot; `Err`: a damaged one.
    fn decode(bytes: &[u8; SLOT_BYTES]) -> Result<Option<Header>, String> {
        if bytes.iter().all(|b| *b == 0) {
            return Ok(None);
        }
        if &bytes[..8] != HEAD_MAGIC {
            return Err("bad magic".into());
        }
        if crc(&bytes[..SLOT_BYTES - 4]) != u32_at(bytes, SLOT_BYTES - 4) {
            return Err("checksum mismatch".into());
        }
        if bytes[40..SLOT_BYTES - 4].iter().any(|b| *b != 0) {
            return Err("reserved bytes are not zero".into());
        }
        Ok(Some(Header {
            generation: u64_at(bytes, 8),
            table_at: u64_at(bytes, 16),
            table_len: u64_at(bytes, 24),
            table_seq: u64_at(bytes, 32),
        }))
    }
}

/// The container version of the single-file store at `path` when it is NEWER than this build
/// reads (format spec 14.1, 15): the preamble's magic matches and its version word is above
/// [`CONTAINER_VERSION`]. `None` for everything else (another file, a directory, a current or
/// unreadable preamble). Reads the 16-byte preamble only, never the container.
pub(crate) fn newer_container_version(path: &Path) -> Option<u32> {
    let file = File::open(path).ok()?;
    preamble_newer(&file)
}

/// See [`newer_container_version`]: the newer version word of the preamble of `file`. Bytes
/// 1..12 are read (byte 0 may be under the writer's mandatory lock on Windows, see
/// [`SingleFileDir::is_single_file`]).
fn preamble_newer(file: &File) -> Option<u32> {
    let mut head = [0u8; 11];
    read_at(file, &mut head, 1).ok()?;
    if head[..7] != PREAMBLE_MAGIC[1..] {
        return None;
    }
    let version = u32::from_le_bytes([head[7], head[8], head[9], head[10]]);
    (version > CONTAINER_VERSION).then_some(version)
}

/// The container version this build reads and writes, for the version guard.
pub(crate) const SUPPORTED_CONTAINER_VERSION: u32 = CONTAINER_VERSION;

fn preamble() -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(PREAMBLE_MAGIC);
    put_u32(&mut out, CONTAINER_VERSION);
    put_u32(&mut out, 0);
    out
}

// ---------------------------------------------------------------------------------------------
// The open container
// ---------------------------------------------------------------------------------------------

/// A container file loaded into memory.
struct Open {
    file: Arc<File>,
    epoch: u64,
    ident: (u64, u64),
    /// The file's size as far as this process knows.
    phys_len: u64,
    /// Where the next record goes (the end of the last valid record).
    end: u64,
    /// Everything before this offset is durable.
    synced: u64,
    next_seq: u64,
    generation: u64,
    table_at: u64,
    table_len: u64,
    /// Record bytes since the latest table.
    tail_bytes: u64,
    tree: Tree,
    damage: Vec<String>,
    warnings: Vec<String>,
    /// No usable table was found (the log was replayed from the start): write one.
    needs_table: bool,
    /// The bytes after `end` are not records (cut at the next write).
    torn_bytes: u64,
}

/// What a scan of records found.
struct Scanned {
    end: u64,
    next_seq: u64,
}

impl Open {
    /// Loads a container file: the newest usable table, then every record after it.
    fn load(file: File) -> io::Result<Open> {
        let meta = file.metadata()?;
        let phys_len = meta.len();
        if phys_len < DATA_START {
            return Err(invalid(format!(
                "not a single-file store: {phys_len} bytes, shorter than its header"
            )));
        }
        // A container of a newer version is never read as this one (format spec 15).
        if let Some(found) = preamble_newer(&file) {
            return Err(invalid(format!(
                "a single-file store of container version {found}, written by a newer \
                 Graphersal (this build reads up to version {CONTAINER_VERSION})"
            )));
        }
        let mut warnings = Vec::new();
        let mut headers = Vec::new();
        let mut generation = 0;
        for (index, at) in SLOT_AT.iter().enumerate() {
            let mut slot = [0u8; SLOT_BYTES];
            read_at(&file, &mut slot, *at)?;
            match Header::decode(&slot) {
                Ok(Some(header)) => {
                    generation = generation.max(header.generation);
                    headers.push(header);
                }
                Ok(None) => {}
                Err(reason) => warnings.push(format!("header slot {index} is damaged ({reason})")),
            }
        }
        if headers.is_empty() && warnings.is_empty() {
            return Err(invalid("not a single-file store: no header"));
        }
        headers.sort_by_key(|h| std::cmp::Reverse(h.generation));
        let mut base = None;
        for header in &headers {
            match read_table(&file, phys_len, header) {
                Ok(tree) => {
                    base = Some((*header, tree));
                    break;
                }
                Err(reason) => warnings.push(format!(
                    "the table of generation {} is damaged ({reason}); an older one and the \
                     records after it were used",
                    header.generation
                )),
            }
        }
        let needs_table = base.is_none();
        let (start, next_seq, tree, header) = match base {
            Some((header, tree)) => (
                header.table_at + header.table_len,
                header.table_seq + 1,
                tree,
                header,
            ),
            None => {
                warnings.push(
                    "no usable table: the names were rebuilt from every record of the file".into(),
                );
                (
                    DATA_START,
                    1,
                    Tree::default(),
                    Header {
                        generation,
                        table_at: DATA_START,
                        table_len: 0,
                        table_seq: 0,
                    },
                )
            }
        };
        let mut open = Open {
            file: Arc::new(file),
            epoch: EPOCH.fetch_add(1, Ordering::Relaxed),
            ident: identity(&meta),
            phys_len,
            end: start,
            synced: start,
            next_seq,
            generation,
            table_at: header.table_at,
            table_len: header.table_len,
            tail_bytes: 0,
            tree,
            damage: Vec::new(),
            warnings,
            needs_table,
            torn_bytes: 0,
        };
        open.scan_from(start, next_seq)?;
        open.synced = open.end;
        Ok(open)
    }

    /// Replays the records from `start` (sequence `expected`) to the end of the file.
    fn scan_from(&mut self, start: u64, expected: u64) -> io::Result<()> {
        let file = self.file.clone();
        let limit = self.phys_len;
        let scanned = scan(
            &file,
            limit,
            start,
            expected,
            &mut self.tree,
            &mut self.damage,
            &mut self.warnings,
        )?;
        self.tail_bytes += scanned.end - start;
        self.end = scanned.end;
        self.next_seq = scanned.next_seq;
        self.torn_bytes = limit - scanned.end;
        Ok(())
    }

    /// Before the first write: refuses on damage, cuts a torn tail, writes a missing table.
    fn prepare_write(&mut self, location: &str) -> io::Result<()> {
        if !self.damage.is_empty() {
            return Err(invalid(format!(
                "the single-file store {location} is damaged ({}); it is read-only: open it \
                 (maintenance mode) and repair it into a new store",
                self.damage.join("; ")
            )));
        }
        if self.end < self.phys_len {
            self.file.set_len(self.end)?;
            self.file.sync_all()?;
            self.phys_len = self.end;
            self.torn_bytes = 0;
        }
        if self.needs_table {
            self.write_table()?;
            self.needs_table = false;
            self.warnings.clear();
        }
        Ok(())
    }

    /// Appends one record (not synced); returns the payload's container offset.
    fn write_record(&mut self, op: &Op, payload: &[u8]) -> io::Result<u64> {
        let head = encode_head(
            self.next_seq,
            self.synced,
            op,
            payload.len() as u64,
            crc(payload),
        );
        let at = self.end;
        let result = write_at(&self.file, &head, at)
            .and_then(|()| write_at(&self.file, payload, at + head.len() as u64));
        if let Err(err) = result {
            // Nothing after `end` counts; cut what the failed write left (best effort).
            let _ = self.file.set_len(at);
            return Err(err);
        }
        let total = head.len() as u64 + payload.len() as u64;
        self.end += total;
        self.phys_len = self.phys_len.max(self.end);
        self.next_seq += 1;
        self.tail_bytes += total;
        Ok(at + head.len() as u64)
    }

    /// Logs an operation (checked first) and applies it.
    fn log(&mut self, op: Op, payload: &[u8]) -> io::Result<()> {
        self.tree.check(&op)?;
        let at = self.write_record(&op, payload)?;
        self.tree.apply(&op, at, payload.len() as u64);
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        if self.synced < self.end {
            self.file.sync_data()?;
            self.synced = self.end;
        }
        Ok(())
    }

    /// Appends the whole table and points the older header slot at it.
    fn write_table(&mut self) -> io::Result<()> {
        let payload = self.tree.encode();
        let seq = self.next_seq;
        let at = self.end;
        self.write_record(&Op::Table, &payload)?;
        let table_len = self.end - at;
        self.sync()?;
        let header = Header {
            generation: self.generation + 1,
            table_at: at,
            table_len,
            table_seq: seq,
        };
        let slot = SLOT_AT[(header.generation % 2) as usize];
        write_at(&self.file, &header.encode(), slot)?;
        self.file.sync_data()?;
        self.generation = header.generation;
        self.table_at = at;
        self.table_len = table_len;
        self.tail_bytes = 0;
        Ok(())
    }

    /// Writes a table when the records since the last one grew large.
    fn maybe_table(&mut self) {
        let due = TABLE_MIN_TAIL.max(TABLE_TAIL_FACTOR.saturating_mul(self.table_len));
        if self.tail_bytes >= due
            && let Err(err) = self.write_table()
        {
            tracing::warn!("writing the table of a single-file store failed: {err}");
        }
    }

    fn check(&self) -> DirCheck {
        DirCheck {
            damage: self.damage.clone(),
            warnings: self.warnings.clone(),
        }
    }
}

/// Reads and decodes the table a header points at.
fn read_table(file: &File, limit: u64, header: &Header) -> Result<Tree, String> {
    let head = match read_head(file, header.table_at, limit).map_err(|e| e.to_string())? {
        Head::Frame(frame) => frame,
        _ => return Err("no valid record where the header points".into()),
    };
    if head.kind != KIND_TABLE || head.seq != header.table_seq || head.total != header.table_len {
        return Err("the record the header points at is not its table".into());
    }
    let len = usize::try_from(head.payload_len).map_err(|_| "a table too large".to_string())?;
    let mut payload = vec![0u8; len];
    read_at(file, &mut payload, header.table_at + head.head_len).map_err(|e| e.to_string())?;
    if crc(&payload) != head.payload_crc {
        return Err("checksum mismatch".into());
    }
    Tree::decode(&payload, header.table_at)
}

/// Replays records from `start` (sequence `expected`) up to `limit` into `tree` (format spec
/// 14.4): stops at a torn tail, records damage in `damage` and goes on after it.
fn scan(
    file: &File,
    limit: u64,
    start: u64,
    mut expected: u64,
    tree: &mut Tree,
    damage: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> io::Result<Scanned> {
    let mut pos = start;
    // The last valid frame of the file (offset, durable end): what tells damage from a torn
    // tail. Found once, on the first problem.
    let mut last: Option<Option<(u64, u64)>> = None;
    let mut durable_after = |pos: u64, from: u64| -> io::Result<(bool, bool)> {
        if last.is_none() {
            last = Some(last_frame(file, from, limit)?);
        }
        Ok(match last.flatten() {
            Some((at, synced)) if at > pos => (true, synced > pos),
            _ => (false, false),
        })
    };
    loop {
        if pos >= limit {
            return Ok(Scanned {
                end: pos,
                next_seq: expected,
            });
        }
        match read_head(file, pos, limit)? {
            Head::Frame(frame) if frame.seq >= expected => {
                if frame.seq > expected {
                    damage.push(format!(
                        "records {expected}..{} are missing before offset {pos}",
                        frame.seq - 1
                    ));
                }
                let payload_at = pos + frame.head_len;
                let intact = frame.payload_len == 0
                    || crc_of(file, payload_at, frame.payload_len)? == frame.payload_crc;
                let mut apply = true;
                if !intact {
                    let (later, durable) = durable_after(pos, pos + frame.total)?;
                    match (frame.kind, durable, later) {
                        // The Store's own checksums locate it, as on a directory.
                        (KIND_APPEND, true, _) => damage.push(format!(
                            "record {} at offset {pos} ({}): its data fails its checksum",
                            frame.seq,
                            op_text(&frame.op)
                        )),
                        // The last write: handed to the Store, which tells a torn record
                        // from damage by its own rules.
                        (KIND_APPEND, false, false) => {}
                        // A table only repeats what the records before it said.
                        (KIND_TABLE, true, _) => {
                            warnings.push(format!(
                                "the table record {} at offset {pos} is damaged (a redundant \
                                 copy; nothing is lost)",
                                frame.seq
                            ));
                            apply = false;
                        }
                        (_, true, _) => {
                            damage.push(format!(
                                "record {} at offset {pos} ({}): it fails its checksum and was \
                                 skipped",
                                frame.seq,
                                op_text(&frame.op)
                            ));
                            apply = false;
                        }
                        _ => {
                            return Ok(Scanned {
                                end: pos,
                                next_seq: expected,
                            });
                        }
                    }
                }
                if apply && let Some(note) = tree.apply(&frame.op, payload_at, frame.payload_len) {
                    damage.push(note);
                }
                pos += frame.total;
                expected = frame.seq + 1;
            }
            Head::Short | Head::PastEnd => {
                return Ok(Scanned {
                    end: pos,
                    next_seq: expected,
                });
            }
            Head::Frame(_) | Head::Bad => {
                let Some(next) = resync(file, pos + 1, limit)? else {
                    if !all_zero(file, pos, limit)? {
                        damage.push(format!(
                            "bytes {pos}..{limit} at the end of the file are not records"
                        ));
                    }
                    return Ok(Scanned {
                        end: pos,
                        next_seq: expected,
                    });
                };
                let (_, durable) = durable_after(pos, next)?;
                if !durable {
                    // Never made durable after them: the torn tail of a crash.
                    return Ok(Scanned {
                        end: pos,
                        next_seq: expected,
                    });
                }
                damage.push(format!("bytes {pos}..{next} are not readable records"));
                pos = next;
            }
        }
    }
}

fn op_text(op: &Op) -> String {
    match op {
        Op::Append { name, offset } => format!("append to {name} at {offset}"),
        Op::Create { name } => format!("create {name}"),
        Op::Put { name } => format!("replace {name}"),
        Op::Truncate { name, len } => format!("truncate {name} to {len}"),
        Op::Mkdir { name } => format!("directory {name}"),
        Op::Rename { from, to } => format!("rename {from} to {to}"),
        Op::Remove { name } => format!("remove {name}"),
        Op::Table => "table".into(),
    }
}

// ---------------------------------------------------------------------------------------------
// The shared container and its locks
// ---------------------------------------------------------------------------------------------

/// Keeps an OS lock held (the locked handle stays open; the guard was forgotten).
struct HeldLock(#[allow(dead_code)] fd_lock::RwLock<File>);

#[derive(Default)]
struct State {
    open: Option<Open>,
    writer: bool,
    backups: usize,
    pruning: bool,
    held: Option<HeldLock>,
}

struct Container {
    path: PathBuf,
    state: Mutex<State>,
    released: Condvar,
}

impl Container {
    fn location(&self) -> String {
        self.path.display().to_string()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Brings the in-memory container up to date with the file (another process may have
    /// appended, compacted or created it). A writer, and a pinned view, trust their state.
    fn refresh(&self, st: &mut State) -> io::Result<()> {
        if st.open.is_some() && (st.writer || st.backups > 0) {
            return Ok(());
        }
        let meta = match fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                st.open = None;
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a file", self.location()),
            ));
        }
        if let Some(open) = st.open.as_mut()
            && open.ident == identity(&meta)
            && open.damage.is_empty()
        {
            let len = meta.len();
            if len == open.phys_len {
                return Ok(());
            }
            if len >= open.end {
                open.phys_len = len;
                let (end, seq) = (open.end, open.next_seq);
                open.scan_from(end, seq)?;
                return Ok(());
            }
        }
        st.open = Some(Open::load(open_file(&self.path)?)?);
        Ok(())
    }

    /// The OS lock of the file: `None` when another process holds it. Retries when the file
    /// was replaced (compacted) between opening and locking it.
    fn os_lock(&self) -> io::Result<Option<HeldLock>> {
        for _ in 0..8 {
            let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
            let locked = identity(&file.metadata()?);
            let mut lock = fd_lock::RwLock::new(file);
            match lock.try_write() {
                Ok(guard) => std::mem::forget(guard),
                Err(_) => return Ok(None),
            }
            // A compaction may have replaced the file between the open and the lock: the lock
            // must be on the file the path names now.
            if cfg!(unix) && identity(&fs::metadata(&self.path)?) != locked {
                continue;
            }
            return Ok(Some(HeldLock(lock)));
        }
        Err(io::Error::other(format!(
            "{}: the file kept changing while it was being locked",
            self.location()
        )))
    }

    /// Creates the file (preamble, an empty table, the first header), atomically: written
    /// under a temporary name, synced, linked to the path only when nothing is there.
    fn create_file(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "store".into());
        let tmp = self.path.with_file_name(format!(
            "{name}.create-{:016x}.tmp",
            EPOCH.fetch_add(1, Ordering::Relaxed) ^ u64::from(std::process::id()) << 32
        ));
        let result = (|| {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            write_empty(&file)?;
            drop(file);
            match fs::hard_link(&tmp, &self.path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                // A file system without hard links: a plain rename (it may replace a file
                // created at the same moment by another process).
                Err(_) if !self.path.exists() => fs::rename(&tmp, &self.path),
                Err(err) => Err(err),
            }
        })();
        let _ = fs::remove_file(&tmp);
        result?;
        sync_parent(&self.path)
    }

    /// Runs `f` on the loaded container, creating the file when `create` and it is missing.
    fn read<R>(&self, f: impl FnOnce(&Open) -> io::Result<R>) -> io::Result<R> {
        let mut st = self.state();
        self.refresh(&mut st)?;
        match st.open.as_ref() {
            Some(open) => f(open),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{}: no such store file", self.location()),
            )),
        }
    }

    /// Runs a change: the file created when missing, the OS lock taken for it unless this
    /// process is the writer, the container prepared (torn tail cut, damage refused).
    fn mutate<R>(&self, f: impl FnOnce(&mut Open) -> io::Result<R>) -> io::Result<R> {
        let mut st = self.state();
        self.refresh(&mut st)?;
        if st.open.is_none() {
            self.create_file()?;
            self.refresh(&mut st)?;
        }
        let _temporary = if st.writer {
            None
        } else {
            let held = self.os_lock()?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "{} is in use by another process (it holds the writer lock)",
                        self.location()
                    ),
                )
            })?;
            // What the other writer appended before it let go.
            let backups = std::mem::take(&mut st.backups);
            let refreshed = self.refresh(&mut st);
            st.backups = backups;
            refreshed?;
            Some(held)
        };
        let location = self.location();
        let open = st
            .open
            .as_mut()
            .ok_or_else(|| io::Error::other(format!("{location}: the file disappeared")))?;
        open.prepare_write(&location)?;
        let result = f(open)?;
        open.maybe_table();
        Ok(result)
    }

    fn sync(&self) -> io::Result<()> {
        let (file, end, epoch) = {
            let st = self.state();
            match st.open.as_ref() {
                Some(open) if open.synced < open.end => (open.file.clone(), open.end, open.epoch),
                _ => return Ok(()),
            }
        };
        file.sync_data()?;
        let mut st = self.state();
        if let Some(open) = st.open.as_mut()
            && open.epoch == epoch
        {
            open.synced = open.synced.max(end);
        }
        Ok(())
    }

    fn append_record(&self, name: &str, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.mutate(|open| {
            if !open.tree.files.contains_key(name) {
                return Err(not_found(name));
            }
            open.log(
                Op::Append {
                    name: name.to_string(),
                    offset,
                },
                bytes,
            )
        })
    }

    fn truncate_file(&self, name: &str, len: u64) -> io::Result<()> {
        self.mutate(|open| {
            let current = open
                .tree
                .files
                .get(name)
                .map(|e| e.len)
                .ok_or_else(|| not_found(name))?;
            if len < current {
                open.log(
                    Op::Truncate {
                        name: name.to_string(),
                        len,
                    },
                    &[],
                )?;
            }
            open.sync()
        })
    }

    /// Copies the live data into a new file next to this one and replaces it.
    fn compact(&self) -> io::Result<Option<DirCompaction>> {
        let mut st = self.state();
        self.refresh(&mut st)?;
        if st.open.is_none() {
            return Ok(None);
        }
        let temporary = if st.writer {
            None
        } else {
            let held = self.os_lock()?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("{} is in use by another process", self.location()),
                )
            })?;
            self.refresh(&mut st)?;
            Some(held)
        };
        let location = self.location();
        let writer = st.writer;
        let Some(open) = st.open.as_mut() else {
            return Ok(None);
        };
        if !open.damage.is_empty() {
            return Err(invalid(format!(
                "the single-file store {location} is damaged; repair it before compacting"
            )));
        }
        let before = open.phys_len;
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "store".into());
        let tmp = self.path.with_file_name(format!(
            "{name}.compact-{:016x}.tmp",
            EPOCH.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| -> io::Result<(Open, Option<HeldLock>)> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            let compacted = write_compacted(&file, &open.file, &open.tree, open.end)?;
            // The writer's lock moves to the new file before the file takes the name.
            let held = if writer {
                let mut lock =
                    fd_lock::RwLock::new(OpenOptions::new().read(true).write(true).open(&tmp)?);
                std::mem::forget(lock.try_write()?);
                Some(HeldLock(lock))
            } else {
                None
            };
            fs::rename(&tmp, &self.path)?;
            sync_parent(&self.path)?;
            let ident = identity(&file.metadata()?);
            let mut fresh = Open::from_compacted(file, compacted);
            fresh.ident = ident;
            Ok((fresh, held))
        })();
        let (fresh, held) = match written {
            Ok(done) => done,
            Err(err) => {
                let _ = fs::remove_file(&tmp);
                return Err(err);
            }
        };
        let after = fresh.phys_len;
        st.open = Some(fresh);
        if let Some(held) = held {
            st.held = Some(held);
        }
        drop(temporary);
        Ok(Some(DirCompaction::new(before, after)))
    }

    /// Removes temporary files a crash left next to the container (an interrupted create or
    /// compaction).
    fn remove_leftovers(&self) {
        let Some(name) = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            return;
        };
        let dir = match self.path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if let Some(rest) = file.strip_prefix(&format!("{name}."))
                && rest.ends_with(".tmp")
                && (rest.starts_with("compact-") || rest.starts_with("create-"))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// What [`write_compacted`] wrote.
struct Compacted {
    tree: Tree,
    end: u64,
    next_seq: u64,
    table_at: u64,
    table_len: u64,
}

impl Open {
    fn from_compacted(file: File, c: Compacted) -> Open {
        Open {
            file: Arc::new(file),
            epoch: EPOCH.fetch_add(1, Ordering::Relaxed),
            ident: (0, 0),
            phys_len: c.end,
            end: c.end,
            synced: c.end,
            next_seq: c.next_seq,
            generation: 1,
            table_at: c.table_at,
            table_len: c.table_len,
            tail_bytes: 0,
            tree: c.tree,
            damage: Vec::new(),
            warnings: Vec::new(),
            needs_table: false,
            torn_bytes: 0,
        }
    }
}

/// Opens the container file read-write, or read-only where writing is not allowed.
fn open_file(path: &Path) -> io::Result<File> {
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => Ok(file),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => File::open(path),
        Err(err) => Err(err),
    }
}

/// Writes a new, empty container into `file` (preamble, an empty table, header slot 1).
fn write_empty(file: &File) -> io::Result<()> {
    let mut start = preamble();
    start.resize(DATA_START as usize, 0);
    write_at(file, &start, 0)?;
    let payload = Tree::default().encode();
    let head = encode_head(1, 0, &Op::Table, payload.len() as u64, crc(&payload));
    write_at(file, &head, DATA_START)?;
    write_at(file, &payload, DATA_START + head.len() as u64)?;
    let header = Header {
        generation: 1,
        table_at: DATA_START,
        table_len: (head.len() + payload.len()) as u64,
        table_seq: 1,
    };
    write_at(file, &header.encode(), SLOT_AT[1])?;
    file.sync_all()
}

/// Writes the live contents of `tree` (whose data is in `source`) into the new file `file`:
/// the directories, every file's data in records of at most [`COPY_CHUNK`], the table, header
/// slot 1; synced.
fn write_compacted(
    file: &File,
    source: &Arc<File>,
    tree: &Tree,
    limit: u64,
) -> io::Result<Compacted> {
    let mut start = preamble();
    start.resize(DATA_START as usize, 0);
    write_at(file, &start, 0)?;
    let mut out = LogWriter {
        file,
        end: DATA_START,
        seq: 1,
        tree: Tree::default(),
    };
    for dir in &tree.dirs {
        out.put(&Op::Mkdir { name: dir.clone() }, &[])?;
    }
    let mut chunk = Vec::new();
    for (name, entry) in &tree.files {
        if entry.len == 0 {
            out.put(&Op::Create { name: name.clone() }, &[])?;
            continue;
        }
        let mut reader = ExtentReader::new(source.clone(), entry.extents.clone(), entry.len, limit);
        let mut offset = 0;
        while offset < entry.len {
            let n = (entry.len - offset).min(COPY_CHUNK) as usize;
            chunk.resize(n, 0);
            reader.read_exact(&mut chunk)?;
            out.put(
                &Op::Append {
                    name: name.clone(),
                    offset,
                },
                &chunk,
            )?;
            offset += n as u64;
        }
    }
    let payload = out.tree.encode();
    let table_at = out.end;
    let table_seq = out.seq;
    out.put(&Op::Table, &payload)?;
    let table_len = out.end - table_at;
    file.sync_all()?;
    let header = Header {
        generation: 1,
        table_at,
        table_len,
        table_seq,
    };
    write_at(file, &header.encode(), SLOT_AT[1])?;
    file.sync_all()?;
    Ok(Compacted {
        tree: out.tree,
        end: out.end,
        next_seq: out.seq,
        table_at,
        table_len,
    })
}

/// Appends records to a new container file (compaction).
struct LogWriter<'f> {
    file: &'f File,
    end: u64,
    seq: u64,
    tree: Tree,
}

impl LogWriter<'_> {
    fn put(&mut self, op: &Op, payload: &[u8]) -> io::Result<()> {
        let head = encode_head(self.seq, 0, op, payload.len() as u64, crc(payload));
        write_at(self.file, &head, self.end)?;
        write_at(self.file, payload, self.end + head.len() as u64)?;
        self.tree
            .apply(op, self.end + head.len() as u64, payload.len() as u64);
        self.end += head.len() as u64 + payload.len() as u64;
        self.seq += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Readers and writers
// ---------------------------------------------------------------------------------------------

/// A sequential reader of a file of the container: its extents as they were when it was
/// opened (the bytes they name are never overwritten), read through 1 MiB windows.
struct ExtentReader {
    file: Arc<File>,
    extents: Vec<Extent>,
    len: u64,
    /// The container's size when the reader was made (windows never read past it).
    limit: u64,
    pos: u64,
    index: usize,
    window: Vec<u8>,
    window_at: u64,
}

impl ExtentReader {
    fn new(file: Arc<File>, extents: Vec<Extent>, len: u64, limit: u64) -> Self {
        // A window never reads past the file's last byte in the container.
        let limit = extents
            .iter()
            .filter(|e| e.at != 0)
            .map(|e| e.at + e.len)
            .max()
            .unwrap_or(0)
            .min(limit.max(DATA_START));
        Self {
            file,
            extents,
            len,
            limit,
            pos: 0,
            index: 0,
            window: Vec::new(),
            window_at: 0,
        }
    }
}

impl Read for ExtentReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        while self
            .extents
            .get(self.index)
            .is_some_and(|e| e.start + e.len <= self.pos)
        {
            self.index += 1;
        }
        let Some(extent) = self.extents.get(self.index).copied() else {
            return Ok(0);
        };
        let within = self.pos - extent.start;
        let n = (extent.len - within).min(buf.len() as u64) as usize;
        if extent.at == 0 {
            buf[..n].fill(0);
        } else {
            let at = extent.at + within;
            let cached =
                at >= self.window_at && at + n as u64 <= self.window_at + self.window.len() as u64;
            if !cached {
                let size = (WINDOW as u64)
                    .max(n as u64)
                    .min(self.limit.saturating_sub(at).max(n as u64));
                self.window.resize(size as usize, 0);
                read_at(&self.file, &mut self.window, at)?;
                self.window_at = at;
            }
            let from = (at - self.window_at) as usize;
            buf[..n].copy_from_slice(&self.window[from..from + n]);
        }
        self.pos += n as u64;
        Ok(n)
    }
}

/// A file of the container open for writing: written bytes are buffered (at most 1 MiB) and
/// become one append record at `flush`, `sync`, `set_len`, when the buffer is full, or when
/// the handle is dropped.
struct ContainerFile {
    container: Arc<Container>,
    name: String,
    /// The logical offset after the buffered bytes.
    at: u64,
    pending: Vec<u8>,
}

impl ContainerFile {
    fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let offset = self.at - self.pending.len() as u64;
        let result = self
            .container
            .append_record(&self.name, offset, &self.pending);
        if result.is_ok() {
            self.pending.clear();
        }
        result
    }
}

impl Write for ContainerFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        self.at += buf.len() as u64;
        if self.pending.len() >= PENDING_BYTES {
            self.flush_pending()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_pending()
    }
}

impl StoreFile for ContainerFile {
    fn sync(&mut self) -> io::Result<()> {
        self.flush_pending()?;
        self.container.sync()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        if self.flush_pending().is_err() {
            // The refused bytes are not written: drop them.
            self.at -= self.pending.len() as u64;
            self.pending.clear();
        }
        self.container.truncate_file(&self.name, len)?;
        self.at = len;
        Ok(())
    }
}

impl Drop for ContainerFile {
    fn drop(&mut self) {
        if let Err(err) = self.flush_pending() {
            tracing::warn!(
                "{} bytes written to {} in the single-file store {} were lost: {err}",
                self.pending.len(),
                self.name,
                self.container.location()
            );
        }
    }
}

/// A lock of the container: released (and, for the writer, a table written) when dropped.
struct ContainerLock {
    container: Arc<Container>,
    kind: LockKind,
}

impl Drop for ContainerLock {
    fn drop(&mut self) {
        let mut st = self.container.state();
        match self.kind {
            LockKind::Writer => {
                // A close: a table, so the next open replays nothing.
                if let Some(open) = st.open.as_mut()
                    && open.damage.is_empty()
                    && open.tail_bytes > 0
                    && open.end == open.phys_len
                    && let Err(err) = open.write_table()
                {
                    tracing::warn!(
                        "writing the table of {} at close failed: {err}",
                        self.container.location()
                    );
                }
                st.writer = false;
                st.held = None;
            }
            LockKind::BackupShared => st.backups = st.backups.saturating_sub(1),
            LockKind::BackupExclusive => st.pruning = false,
        }
        drop(st);
        self.container.released.notify_all();
    }
}

// ---------------------------------------------------------------------------------------------
// The public backend
// ---------------------------------------------------------------------------------------------

/// A [`StoreDir`] that keeps a whole Store in ONE file (format spec section 14; native only):
/// a log-structured container of appended records with two header slots pointing at the
/// latest name table. Every Store operation works on it as on a directory; removed data is
/// garbage until [`StoreDir::compact`] (the Store's [`compact`](super::Store::compact))
/// rewrites the live data into a new file next to it and replaces the file atomically (the
/// disk needs room for the live data meanwhile, as SQLite's `VACUUM`).
///
/// - **One writer**: the writer lock is the OS lock on the file itself; a process without
///   it changes the file only under a short-lived lock per change.
/// - **Relocatable**: the file holds no path; move or copy it while no process has it open.
/// - **Crash-safe**: records are appended, never overwritten; the next write cuts a torn last
///   write; damage of the container's own records opens the Store in maintenance mode
///   ([`StoreDir::check`]).
///
/// Every `SingleFileDir` of one path in a process shares one in-memory container (the
/// registry is keyed by the absolute path, links are not resolved).
#[derive(Clone)]
pub struct SingleFileDir {
    inner: Arc<Container>,
}

impl std::fmt::Debug for SingleFileDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleFileDir")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl SingleFileDir {
    /// The single-file store at `path` (it need not exist yet: the first write creates it).
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let key = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let mut registry = lock(registry());
        registry.retain(|_, weak| weak.strong_count() > 0);
        if let Some(inner) = registry.get(&key).and_then(Weak::upgrade) {
            return Self { inner };
        }
        let inner = Arc::new(Container {
            path: path.to_path_buf(),
            state: Mutex::new(State::default()),
            released: Condvar::new(),
        });
        registry.insert(key, Arc::downgrade(&inner));
        Self { inner }
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Whether `path` is an existing single-file store (its preamble is read). Bytes 1..8 of
    /// the magic are compared: on Windows the writer's OS lock covers byte 0 (`LockFileEx` of
    /// one byte at offset 0) and is mandatory, so another process cannot read it while the
    /// store is open. Nothing at `path`, a directory, or a file shorter than the preamble is
    /// `Ok(false)`; a file that cannot be read (no permission, an I/O error) is
    /// [`PersistError::Io`], never "not a store".
    pub fn is_single_file(path: impl AsRef<Path>) -> Result<bool, PersistError> {
        const CONTEXT: &str = "read the preamble of a single-file store";
        let path = path.as_ref();
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Ok(false),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(PersistError::io(CONTEXT, err)),
        }
        let file = File::open(path).map_err(|err| PersistError::io(CONTEXT, err))?;
        let mut magic = [0u8; 7];
        match read_at(&file, &mut magic, 1) {
            Ok(()) => Ok(magic == PREAMBLE_MAGIC[1..]),
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(err) => Err(PersistError::io(CONTEXT, err)),
        }
    }
}

impl StoreDir for SingleFileDir {
    fn location(&self) -> String {
        self.inner.location()
    }

    fn local_path(&self) -> Option<&Path> {
        Some(&self.inner.path)
    }

    fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>> {
        check_name(name)?;
        let mut st = self.inner.state();
        self.inner.refresh(&mut st)?;
        let Some(open) = st.open.as_ref() else {
            return Ok(None);
        };
        Ok(if open.tree.files.contains_key(name) {
            Some(EntryKind::File)
        } else if open.tree.is_dir(name) {
            Some(EntryKind::Dir)
        } else {
            None
        })
    }

    fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>> {
        check_name(dir)?;
        self.inner.read(|open| {
            let tree = &open.tree;
            if !tree.is_dir(dir) {
                return Err(not_found(dir));
            }
            let prefix = if dir.is_empty() {
                String::new()
            } else {
                format!("{dir}/")
            };
            let direct = |name: &String| {
                name.strip_prefix(&prefix)
                    .filter(|rest| !rest.is_empty() && !rest.contains('/'))
                    .map(str::to_string)
            };
            let mut out: Vec<DirEntry> = tree
                .files
                .keys()
                .filter_map(direct)
                .map(|name| DirEntry::new(name, EntryKind::File))
                .collect();
            out.extend(
                tree.dirs
                    .iter()
                    .filter_map(direct)
                    .map(|name| DirEntry::new(name, EntryKind::Dir)),
            );
            Ok(out)
        })
    }

    fn file_len(&self, name: &str) -> io::Result<u64> {
        self.inner.read(|open| {
            open.tree
                .files
                .get(name)
                .map(|e| e.len)
                .ok_or_else(|| not_found(name))
        })
    }

    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        let (file, extents, len, limit) = self.inner.read(|open| {
            let entry = open.tree.files.get(name).ok_or_else(|| not_found(name))?;
            if entry.len > max {
                return Err(invalid(format!("{name} is larger than {max} bytes")));
            }
            Ok((
                open.file.clone(),
                entry.extents.clone(),
                entry.len,
                open.end,
            ))
        })?;
        let mut out = vec![0u8; usize::try_from(len).map_err(|_| invalid("file too large"))?];
        ExtentReader::new(file, extents, len, limit).read_exact(&mut out)?;
        Ok(out)
    }

    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let (file, extents, limit) = self.inner.read(|open| {
            let entry = open.tree.files.get(name).ok_or_else(|| not_found(name))?;
            if offset
                .checked_add(len as u64)
                .is_none_or(|end| end > entry.len)
            {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{name} is shorter than {offset} + {len} bytes"),
                ));
            }
            Ok((open.file.clone(), entry.slice(offset, len as u64), open.end))
        })?;
        let mut out = vec![0u8; len];
        let rebased: Vec<Extent> = extents
            .iter()
            .map(|e| Extent {
                start: e.start - offset,
                ..*e
            })
            .collect();
        ExtentReader::new(file, rebased, len as u64, limit).read_exact(&mut out)?;
        Ok(out)
    }

    fn open_read(&self, name: &str) -> io::Result<Box<dyn Read + Send>> {
        let (file, extents, len, limit) = self.inner.read(|open| {
            let entry = open.tree.files.get(name).ok_or_else(|| not_found(name))?;
            Ok((
                open.file.clone(),
                entry.extents.clone(),
                entry.len,
                open.end,
            ))
        })?;
        Ok(Box::new(ExtentReader::new(file, extents, len, limit)))
    }

    fn create_dir_all(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        self.inner.mutate(|open| {
            if name.is_empty() || open.tree.dirs.contains(name) {
                return Ok(());
            }
            open.log(
                Op::Mkdir {
                    name: name.to_string(),
                },
                &[],
            )
        })
    }

    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        check_name(name)?;
        self.inner.mutate(|open| {
            if exclusive && open.tree.files.contains_key(name) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{name} exists"),
                ));
            }
            open.log(
                Op::Create {
                    name: name.to_string(),
                },
                &[],
            )
        })?;
        Ok(Box::new(ContainerFile {
            container: self.inner.clone(),
            name: name.to_string(),
            at: 0,
            pending: Vec::new(),
        }))
    }

    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        check_name(name)?;
        self.inner.read(|open| {
            open.tree
                .files
                .get(name)
                .map(|_| ())
                .ok_or_else(|| not_found(name))
        })?;
        Ok(Box::new(ContainerFile {
            container: self.inner.clone(),
            name: name.to_string(),
            at: offset,
            pending: Vec::new(),
        }))
    }

    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        check_name(name)?;
        self.inner.mutate(|open| {
            let offset = open.tree.files.get(name).map_or(0, |e| e.len);
            open.log(
                Op::Append {
                    name: name.to_string(),
                    offset,
                },
                bytes,
            )?;
            open.sync()
        })
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        check_name(name)?;
        self.inner.mutate(|open| {
            open.log(
                Op::Put {
                    name: name.to_string(),
                },
                bytes,
            )?;
            open.sync()
        })
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        check_name(from)?;
        check_name(to)?;
        self.inner.mutate(|open| {
            open.log(
                Op::Rename {
                    from: from.to_string(),
                    to: to.to_string(),
                },
                &[],
            )
        })
    }

    fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
        check_name(name)?;
        self.inner.truncate_file(name, len)
    }

    fn remove_file(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        self.inner.mutate(|open| {
            if !open.tree.files.contains_key(name) {
                return Err(not_found(name));
            }
            open.log(
                Op::Remove {
                    name: name.to_string(),
                },
                &[],
            )
        })
    }

    fn remove_tree(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        if name.is_empty() {
            // The root: the whole store, i.e. the file (what a failed create leaves).
            let mut st = self.inner.state();
            if st.writer {
                return Err(io::Error::other(format!(
                    "{}: the store is open",
                    self.inner.location()
                )));
            }
            st.open = None;
            return match fs::remove_file(&self.inner.path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other.and_then(|()| sync_parent(&self.inner.path)),
            };
        }
        if self.entry_kind(name)?.is_none() {
            return Ok(());
        }
        self.inner.mutate(|open| {
            open.log(
                Op::Remove {
                    name: name.to_string(),
                },
                &[],
            )
        })
    }

    fn sync_dir(&self, _: &str) -> io::Result<()> {
        self.inner.sync()
    }

    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
        let mut st = self.inner.state();
        match kind {
            LockKind::Writer => {
                if st.writer {
                    return Ok(None);
                }
                if !self.inner.path.exists() {
                    self.inner.create_file()?;
                }
                let Some(held) = self.inner.os_lock()? else {
                    return Ok(None);
                };
                self.inner.refresh(&mut st)?;
                self.inner.remove_leftovers();
                st.writer = true;
                st.held = Some(held);
            }
            LockKind::BackupExclusive => {
                if st.pruning || st.backups > 0 {
                    return Ok(None);
                }
                st.pruning = true;
            }
            LockKind::BackupShared => {
                while st.pruning {
                    st = self
                        .inner
                        .released
                        .wait(st)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                // The view is frozen from here on: bring it up to date first.
                if st.backups == 0 {
                    self.inner.refresh(&mut st)?;
                }
                st.backups += 1;
            }
        }
        drop(st);
        Ok(Some(DirLockGuard::new(ContainerLock {
            container: self.inner.clone(),
            kind,
        })))
    }

    fn space_usage(&self) -> Option<SpaceUsage> {
        self.inner
            .read(|open| {
                Ok(SpaceUsage::new(
                    open.phys_len,
                    open.tree.compacted_size().min(open.phys_len),
                ))
            })
            .ok()
    }

    fn compact(&self) -> io::Result<Option<DirCompaction>> {
        self.inner.compact()
    }

    fn free_space(&self) -> Option<u64> {
        // `StoreDir::free_space` answers `Option` until the trait itself returns a `Result`
        // (its methods change separately, part B of the result alignment): until then an
        // unknown free space is all the trait can say.
        free_space_at(&self.inner.path).ok().flatten()
    }

    fn check(&self) -> DirCheck {
        self.inner
            .read(|open| Ok(open.check()))
            .unwrap_or_else(|err| DirCheck {
                damage: vec![format!("the file cannot be read: {err}")],
                warnings: Vec::new(),
            })
    }
}

impl super::IntoStoreDir for SingleFileDir {
    fn into_store_dir(self) -> Arc<dyn StoreDir> {
        Arc::new(self)
    }
}

impl super::IntoStoreDir for &SingleFileDir {
    fn into_store_dir(self) -> Arc<dyn StoreDir> {
        Arc::new(self.clone())
    }
}

#[cfg(test)]
mod tests;
