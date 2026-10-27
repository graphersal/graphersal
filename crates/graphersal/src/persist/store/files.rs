//! Files of a Store directory (format spec sections 4, 5 and 11): the `GRAPH` file and its copy,
//! the OS lock, the `INTENT` file, file names, and the write ordering helpers (file, fsync,
//! rename, fsync directory).

use crate::persist::StoreFailure;
use std::io::Read;
use std::sync::Arc;

use super::super::codec::{crc, put_i64, put_u32, put_u64};
pub(crate) use super::super::dir::EntryKind;
use super::super::dir::{DirLockGuard, LockKind, StoreDir, StoreFile};
use super::super::error::PersistError;
use crate::value::physical;
use crate::value::{ElementProperty, PropertyMap};

/// Magic of the `GRAPH` file.
const GRAPH_MAGIC: &[u8; 8] = b"GRSLGRPH";
/// Size of the fixed part of the `GRAPH` file.
pub(crate) const GRAPH_FIXED_BYTES: usize = 256;
/// Size of one lineage entry.
const LINEAGE_ENTRY_BYTES: usize = 32;

pub(crate) const GRAPH_FILE: &str = "GRAPH";
pub(crate) const GRAPH_COPY: &str = "GRAPH.copy";

pub(crate) const INTENT_FILE: &str = "INTENT";
pub(crate) const MARKS_FILE: &str = "marks";
pub(crate) const SNAPSHOTS_DIR: &str = "snapshots";
pub(crate) const WAL_DIR: &str = "wal";
pub(crate) const WAL_EXTENSION: &str = "wal";
pub(crate) const TMP_PREFIX: &str = "tmp-";

/// A name in a store directory ([`StoreDir`]) with the directory it belongs to: the Store's
/// internal stand-in for a path. `join`, `parent` and `file_name` work on the logical name
/// (parts separated by `/`, `""` is the root); every file operation goes to the directory.
#[derive(Clone)]
pub(crate) struct DirPath {
    dir: Arc<dyn StoreDir>,
    name: String,
}

impl std::fmt::Debug for DirPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

impl std::fmt::Display for DirPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

impl DirPath {
    /// The root of `dir`.
    pub(crate) fn root(dir: Arc<dyn StoreDir>) -> Self {
        Self {
            dir,
            name: String::new(),
        }
    }

    /// The directory this name belongs to.
    pub(crate) fn store_dir(&self) -> &Arc<dyn StoreDir> {
        &self.dir
    }

    /// The logical name (`""`: the root).
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The name `part` (one or more parts) below this one.
    pub(crate) fn join(&self, part: impl AsRef<str>) -> Self {
        let part = part.as_ref();
        Self {
            dir: self.dir.clone(),
            name: if self.name.is_empty() {
                part.to_string()
            } else if part.is_empty() {
                self.name.clone()
            } else {
                format!("{}/{part}", self.name)
            },
        }
    }

    /// The parent (`None` for the root).
    pub(crate) fn parent(&self) -> Option<Self> {
        if self.name.is_empty() {
            return None;
        }
        Some(Self {
            dir: self.dir.clone(),
            name: self
                .name
                .rsplit_once('/')
                .map_or(String::new(), |(dir, _)| dir.to_string()),
        })
    }

    /// The last part of the name.
    pub(crate) fn file_name(&self) -> &str {
        self.name
            .rsplit_once('/')
            .map_or(self.name.as_str(), |(_, last)| last)
    }

    /// This name relative to `base` (`base` must be a prefix; otherwise the whole name).
    pub(crate) fn relative_to(&self, base: &DirPath) -> String {
        if base.name.is_empty() {
            return self.name.clone();
        }
        self.name
            .strip_prefix(&base.name)
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(&self.name)
            .to_string()
    }

    /// The location for diagnostics: the directory's location and the name.
    pub(crate) fn display(&self) -> String {
        let location = self.dir.location();
        if self.name.is_empty() {
            location
        } else {
            format!("{location}/{}", self.name)
        }
    }

    pub(crate) fn kind(&self) -> Option<EntryKind> {
        self.dir.entry_kind(&self.name).ok().flatten()
    }

    /// [`Self::kind`] with the read error kept (`Ok(None)`: nothing there).
    pub(crate) fn try_kind(&self) -> std::io::Result<Option<EntryKind>> {
        self.dir.entry_kind(&self.name)
    }

    pub(crate) fn exists(&self) -> bool {
        self.kind().is_some()
    }

    pub(crate) fn is_dir(&self) -> bool {
        self.kind() == Some(EntryKind::Dir)
    }

    /// The children: `(name, kind)`.
    pub(crate) fn list(&self) -> std::io::Result<Vec<(String, EntryKind)>> {
        Ok(self
            .dir
            .list(&self.name)?
            .into_iter()
            .map(|e| (e.name, e.kind))
            .collect())
    }

    pub(crate) fn len(&self) -> std::io::Result<u64> {
        self.dir.file_len(&self.name)
    }

    pub(crate) fn read_range(&self, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        self.dir.read_range(&self.name, offset, len)
    }

    pub(crate) fn open_read(&self) -> std::io::Result<Box<dyn Read + Send>> {
        self.dir.open_read(&self.name)
    }

    pub(crate) fn create_dir_all(&self) -> std::io::Result<()> {
        self.dir.create_dir_all(&self.name)
    }

    pub(crate) fn create(&self, exclusive: bool) -> std::io::Result<Box<dyn StoreFile>> {
        self.dir.create(&self.name, exclusive)
    }

    pub(crate) fn open_write(&self, offset: u64) -> std::io::Result<Box<dyn StoreFile>> {
        self.dir.open_write(&self.name, offset)
    }

    pub(crate) fn append(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.dir.append(&self.name, bytes)
    }

    /// Replaces this file atomically and durably.
    pub(crate) fn write_atomic(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.dir.write_atomic(&self.name, bytes)
    }

    /// Renames to `to` (a name of the same directory).
    pub(crate) fn rename_to(&self, to: &DirPath) -> std::io::Result<()> {
        self.dir.rename(&self.name, &to.name)
    }

    pub(crate) fn truncate(&self, len: u64) -> std::io::Result<()> {
        self.dir.truncate(&self.name, len)
    }

    pub(crate) fn remove_file(&self) -> std::io::Result<()> {
        self.dir.remove_file(&self.name)
    }

    /// Removes a file or a directory tree; a missing one is fine.
    pub(crate) fn remove(&self) -> std::io::Result<()> {
        self.dir.remove_tree(&self.name)
    }
}

/// One entry of a store's lineage chain (`GRAPH` file, format spec section 5): a graph lineage
/// and where it branched from its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LineageEntry {
    /// The lineage id ([`PersistentStorage::graph_id`](crate::graph::PersistentStorage::graph_id)).
    pub graph_id: u128,
    /// The parent's commit sequence number this lineage branched at (`0`: the root lineage).
    pub branched_at: u64,
    /// When it branched (or the store was created), microseconds since the Unix epoch, UTC.
    pub time: i64,
}

/// The decoded `GRAPH` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphFile {
    pub(crate) version: u32,
    /// `false`: a writer has the store open, or crashed.
    pub(crate) clean: bool,
    pub(crate) graph_id: u128,
    pub(crate) created_at: i64,
    pub(crate) chunk_bytes: u32,
    pub(crate) flags: u32,
    /// Newest first; the first entry is `graph_id`'s.
    pub(crate) lineage: Vec<LineageEntry>,
    /// A refused WAL record that could not be cut off (format spec 9.7): the WAL segment (its
    /// first commit sequence number) and the offset where the valid WAL ends. Everything from
    /// there on is never replayed; the next read-write open cuts it and clears the field.
    pub(crate) wal_cut: Option<(u64, u64)>,
    /// The directory is a backup (format spec 12.6): read-only until it is restored.
    pub(crate) backup: Option<BackupMarker>,
    /// The first commit sequence number (the name) of the newest WAL segment, recorded when a
    /// segment is started (format spec 9.8): that segment must exist at the next open.
    pub(crate) wal_head: Option<u64>,
    /// The last commit when the store was closed cleanly (format spec 9.8): a clean open must
    /// reach it.
    pub(crate) closed_at: Option<u64>,
    /// What the store does when damage is found on disk while it is open (format spec 5.2
    /// bit 5): fixed when the store is created, carried by every copy of it.
    pub(crate) damage_policy: DamagePolicy,
    /// The store's identity (format spec 5.1 offset 100; never zero): random, made by a
    /// create; a fork, attic fork, repair or conversion gets a new one, while an in-place
    /// rollback, an attic restore and a restored backup keep it. A backup records its
    /// source's, and an increment requires it to be equal (12.7).
    pub(crate) store_id: u128,
}

/// What an open store does when damage is found ON DISK while it is open (by a backup, a
/// checkpoint, a verification; format spec 5.2 bit 5, 13.3). Fixed when the store is created
/// ([`StoreOptions::with_damage_policy`](super::StoreOptions::with_damage_policy)) and never
/// changed afterwards: forks, repairs, conversions, backups and restores carry it. It does not
/// apply to damage found when the store is OPENED: a store that cannot be loaded intact always
/// opens in maintenance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[non_exhaustive]
pub enum DamagePolicy {
    /// The default: the store switches to read-only (commits, marks, checkpoints, prune,
    /// rollback refused) with the damage as the reason; reading and a backup from memory
    /// ([`Store::backup_from_memory`](super::Store::backup_from_memory)) keep working.
    #[default]
    Maintenance,
    /// The store keeps accepting commits; the damage stays reported (store info, a warning in
    /// the log) for as long as the store is open.
    Continue,
}

impl DamagePolicy {
    /// The policy's name: `"maintenance"` or `"continue"` (the CLI's `--on-damage` value).
    pub fn name(self) -> &'static str {
        match self {
            DamagePolicy::Maintenance => "maintenance",
            DamagePolicy::Continue => "continue",
        }
    }

    /// Parses a policy name (`"maintenance"`, `"continue"`; ASCII case-insensitive).
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "maintenance" => Ok(DamagePolicy::Maintenance),
            "continue" => Ok(DamagePolicy::Continue),
            other => Err(format!(
                "unknown damage policy {other:?}: expected \"maintenance\" (the default) or \
                 \"continue\""
            )),
        }
    }
}

impl std::fmt::Display for DamagePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The marker of a backup directory in its `GRAPH` file (format spec 12.6): "a backup of this
/// graph lineage up to commit N, taken at T". A marked directory opens read-only
/// ([`Store::open_backup`](super::Store::open_backup), [`Store::open_read_only`](super::Store::open_read_only));
/// [`Store::restore_backup`](super::Store::restore_backup) clears the marker and makes it the
/// live store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BackupMarker {
    /// The last commit the backup holds (of the lineage in the `GRAPH` file's `graph_id`).
    pub commit_seq: u64,
    /// When the backup (its last increment) was taken, microseconds since the Unix epoch, UTC.
    pub taken_at: i64,
}

/// `GRAPH` flags bit 1: the `wal_cut` fields at offsets 52 and 60 are set.
const FLAG_WAL_CUT: u32 = 1 << 1;
/// `GRAPH` flags bit 2: a backup; its commit at offset 68, its time at offset 76.
const FLAG_BACKUP: u32 = 1 << 2;
/// `GRAPH` flags bit 3: offset 84 holds the newest WAL segment's first commit (`wal_head`).
const FLAG_WAL_HEAD: u32 = 1 << 3;
/// `GRAPH` flags bit 4: offset 92 holds the last commit of a clean close (`closed_at`).
const FLAG_CLOSED_AT: u32 = 1 << 4;
/// `GRAPH` flags bit 5: the damage policy is [`DamagePolicy::Continue`] (clear: the default,
/// [`DamagePolicy::Maintenance`]; a reader that does not know the bit keeps the safe default).
const FLAG_CONTINUE_ON_DAMAGE: u32 = 1 << 5;
/// The flags this build interprets itself (the others are kept as found).
const KNOWN_FLAGS: u32 =
    FLAG_WAL_CUT | FLAG_BACKUP | FLAG_WAL_HEAD | FLAG_CLOSED_AT | FLAG_CONTINUE_ON_DAMAGE;

impl GraphFile {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(GRAPH_FIXED_BYTES + 36 * self.lineage.len());
        out.extend_from_slice(GRAPH_MAGIC);
        put_u32(&mut out, self.version);
        put_u32(&mut out, if self.clean { 0 } else { 1 });
        out.extend_from_slice(&self.graph_id.to_be_bytes());
        put_i64(&mut out, self.created_at);
        put_u32(&mut out, self.lineage.len() as u32);
        put_u32(&mut out, self.chunk_bytes);
        let (flags, (cut_segment, cut_offset)) = match self.wal_cut {
            Some(cut) => (self.flags | FLAG_WAL_CUT, cut),
            None => (self.flags & !FLAG_WAL_CUT, (0, 0)),
        };
        let (flags, (backup_commit, backup_time)) = match self.backup {
            Some(marker) => (flags | FLAG_BACKUP, (marker.commit_seq, marker.taken_at)),
            None => (flags & !FLAG_BACKUP, (0, 0)),
        };
        let flags = match self.wal_head {
            Some(_) => flags | FLAG_WAL_HEAD,
            None => flags & !FLAG_WAL_HEAD,
        };
        let flags = match self.closed_at {
            Some(_) => flags | FLAG_CLOSED_AT,
            None => flags & !FLAG_CLOSED_AT,
        };
        let flags = match self.damage_policy {
            DamagePolicy::Continue => flags | FLAG_CONTINUE_ON_DAMAGE,
            DamagePolicy::Maintenance => flags & !FLAG_CONTINUE_ON_DAMAGE,
        };
        put_u32(&mut out, flags);
        put_u64(&mut out, cut_segment);
        put_u64(&mut out, cut_offset);
        put_u64(&mut out, backup_commit);
        put_i64(&mut out, backup_time);
        put_u64(&mut out, self.wal_head.unwrap_or(0));
        put_u64(&mut out, self.closed_at.unwrap_or(0));
        out.extend_from_slice(&self.store_id.to_be_bytes());
        out.resize(GRAPH_FIXED_BYTES - 4, 0);
        let checksum = crc(&out);
        put_u32(&mut out, checksum);
        let start = out.len();
        for entry in &self.lineage {
            out.extend_from_slice(&entry.graph_id.to_be_bytes());
            put_u64(&mut out, entry.branched_at);
            put_i64(&mut out, entry.time);
        }
        let checksum = crc(&out[start..]);
        put_u32(&mut out, checksum);
        out
    }

    /// The state word of an intact `GRAPH` file of this store format when it is a value this
    /// version does not know (`0` clean and `1` open are known), `None` otherwise.
    pub(crate) fn unknown_state(bytes: &[u8]) -> Option<u32> {
        if bytes.len() < GRAPH_FIXED_BYTES + 4 || &bytes[..8] != GRAPH_MAGIC {
            return None;
        }
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        if crc(&bytes[..GRAPH_FIXED_BYTES - 4]) != word(GRAPH_FIXED_BYTES - 4)
            || word(8) != STORE_VERSION
        {
            return None;
        }
        Some(word(12)).filter(|state| *state > 1)
    }

    /// Decodes and checks a `GRAPH` file; an older version is refused here, a newer one is
    /// returned as found
    /// (the caller guards it).
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < GRAPH_FIXED_BYTES + 4 {
            return Err("shorter than its fixed part".into());
        }
        if &bytes[..8] != GRAPH_MAGIC {
            return Err("not a GRAPH file (bad magic)".into());
        }
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let long = |at: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[at..at + 8]);
            b
        };
        let id = |at: usize| {
            let mut b = [0u8; 16];
            b.copy_from_slice(&bytes[at..at + 16]);
            u128::from_be_bytes(b)
        };
        if crc(&bytes[..GRAPH_FIXED_BYTES - 4]) != word(GRAPH_FIXED_BYTES - 4) {
            return Err("checksum mismatch of the fixed part".into());
        }
        // The version decides the layout: an older one is classified before anything that
        // version did not have (the store id), see `older_version`.
        let version = word(8);
        if version == 0 {
            return Err("unknown format version 0".into());
        }
        if version < STORE_VERSION {
            return Err(format!("older format version {version}"));
        }
        let count = word(40) as usize;
        let expected = count
            .checked_mul(LINEAGE_ENTRY_BYTES)
            .and_then(|n| n.checked_add(GRAPH_FIXED_BYTES + 4));
        if expected != Some(bytes.len()) {
            return Err(format!(
                "{count} lineage entries do not match the file size of {} bytes",
                bytes.len()
            ));
        }
        let entries = &bytes[GRAPH_FIXED_BYTES..bytes.len() - 4];
        if crc(entries) != word(bytes.len() - 4) {
            return Err("checksum mismatch of the lineage entries".into());
        }
        let state = word(12);
        if state > 1 {
            return Err(format!("unknown state {state}"));
        }
        let mut lineage = Vec::with_capacity(count);
        for i in 0..count {
            let at = GRAPH_FIXED_BYTES + i * LINEAGE_ENTRY_BYTES;
            lineage.push(LineageEntry {
                graph_id: id(at),
                branched_at: u64::from_le_bytes(long(at + 16)),
                time: i64::from_le_bytes(long(at + 24)),
            });
        }
        let graph_id = id(16);
        if lineage.first().map(|e| e.graph_id) != Some(graph_id) {
            return Err("the first lineage entry is not the graph's lineage".into());
        }
        let store_id = id(100);
        if store_id == 0 {
            return Err("no store id (offset 100 is zero)".into());
        }
        let flags = word(48);
        Ok(GraphFile {
            version,
            clean: state == 0,
            graph_id,
            created_at: i64::from_le_bytes(long(32)),
            chunk_bytes: word(44),
            flags: flags & !KNOWN_FLAGS,
            lineage,
            wal_cut: (flags & FLAG_WAL_CUT != 0)
                .then(|| (u64::from_le_bytes(long(52)), u64::from_le_bytes(long(60)))),
            backup: (flags & FLAG_BACKUP != 0).then(|| BackupMarker {
                commit_seq: u64::from_le_bytes(long(68)),
                taken_at: i64::from_le_bytes(long(76)),
            }),
            wal_head: (flags & FLAG_WAL_HEAD != 0).then(|| u64::from_le_bytes(long(84))),
            closed_at: (flags & FLAG_CLOSED_AT != 0).then(|| u64::from_le_bytes(long(92))),
            damage_policy: if flags & FLAG_CONTINUE_ON_DAMAGE != 0 {
                DamagePolicy::Continue
            } else {
                DamagePolicy::Maintenance
            },
            store_id,
        })
    }

    /// The version word of a `GRAPH` file of an OLDER store format (`1..STORE_VERSION`): the
    /// magic and the checksum of the fixed part are valid, so it is a store file this build
    /// does not read, not damage. `None` for everything else.
    pub(crate) fn older_version(bytes: &[u8]) -> Option<u32> {
        Self::intact_version(bytes).filter(|version| (1..STORE_VERSION).contains(version))
    }

    /// The version word of a `GRAPH` file of a NEWER store format (above `STORE_VERSION`):
    /// the magic and the checksum of the fixed part are valid, so it was written by a newer
    /// Graphersal; it is refused with [`PersistError::UnsupportedVersion`], never taken for
    /// damage (format spec 15). `None` for everything else.
    pub(crate) fn newer_version(bytes: &[u8]) -> Option<u32> {
        Self::intact_version(bytes).filter(|version| *version > STORE_VERSION)
    }

    /// The version word of a `GRAPH` file whose magic and fixed-part checksum are valid.
    fn intact_version(bytes: &[u8]) -> Option<u32> {
        if bytes.len() < GRAPH_FIXED_BYTES + 4 || &bytes[..8] != GRAPH_MAGIC {
            return None;
        }
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        if crc(&bytes[..GRAPH_FIXED_BYTES - 4]) != word(GRAPH_FIXED_BYTES - 4) {
            return None;
        }
        Some(word(8))
    }

    /// Whether `graph_id` is a lineage of this store (the current one or an ancestor).
    pub(crate) fn on_chain(&self, graph_id: u128) -> bool {
        self.lineage.iter().any(|entry| entry.graph_id == graph_id)
    }

    /// The chain as recovery takes it: `(graph_id, last commit it may hold)`, newest first.
    pub(crate) fn chain(&self) -> Vec<(u128, u64)> {
        self.lineage
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let valid = match i {
                    0 => u64::MAX,
                    i => self.lineage[i - 1].branched_at,
                };
                (entry.graph_id, valid)
            })
            .collect()
    }
}

/// What reading `GRAPH` and `GRAPH.copy` found.
pub(crate) struct GraphFileRead {
    pub(crate) file: GraphFile,
    /// One copy was missing, damaged or older: both are rewritten at the next write.
    pub(crate) repaired: Option<String>,
}

/// Reads `GRAPH` and `GRAPH.copy`, uses a valid one (both valid and different: the longer
/// lineage wins, then the copy, which is written first).
///
/// Neither valid but one a `GRAPH` of an older store format: [`PersistError::OlderFormat`]
/// (never damage, never "not a store").
pub(crate) fn read_graph_file(dir: &DirPath) -> Result<GraphFileRead, PersistError> {
    let (main, copy) = decode_graph_copies(dir)?;
    match (main, copy) {
        (Ok(main), Ok(copy)) if main == copy => Ok(GraphFileRead {
            file: main,
            repaired: None,
        }),
        (Ok(main), Ok(copy)) => {
            let use_copy = copy.lineage.len() >= main.lineage.len();
            let file = if use_copy { copy } else { main };
            Ok(GraphFileRead {
                file,
                repaired: Some(format!(
                    "GRAPH and GRAPH.copy differ (an interrupted rewrite); {} is used",
                    if use_copy { "GRAPH.copy" } else { "GRAPH" }
                )),
            })
        }
        (Ok(file), Err(err)) => Ok(GraphFileRead {
            file,
            repaired: Some(format!("GRAPH.copy is not usable ({err}); GRAPH is used")),
        }),
        (Err(err), Ok(file)) => Ok(GraphFileRead {
            file,
            repaired: Some(format!("GRAPH is not usable ({err}); GRAPH.copy is used")),
        }),
        (Err(main), Err(_)) => Err(PersistError::NotAStore {
            path: dir.display().to_string(),
            reason: format!("no valid GRAPH file ({main})"),
        }),
    }
}

/// One decoded `GRAPH` copy, or why it is not usable.
pub(crate) type Decoded = Result<GraphFile, String>;

/// `GRAPH` and `GRAPH.copy` decoded, each with why it is not usable. Neither usable but one
/// a valid `GRAPH` of an older store format: [`PersistError::OlderFormat`], classified by its
/// version word before anything that version did not have (format spec 15).
pub(crate) fn decode_graph_copies(dir: &DirPath) -> Result<(Decoded, Decoded), PersistError> {
    let mut older = None;
    let mut newer = None;
    let mut read = |name: &str| -> Result<GraphFile, String> {
        let bytes = read_bounded(&dir.join(name), 64 << 20).map_err(|err| err.to_string())?;
        newer = newer.max(GraphFile::newer_version(&bytes));
        GraphFile::decode(&bytes).inspect_err(|_| {
            older = older.or(GraphFile::older_version(&bytes));
        })
    };
    let main = read(GRAPH_FILE);
    let copy = read(GRAPH_COPY);
    // Either copy of a NEWER store format (an upgrade by a newer Graphersal writes the copy
    // first): the store is refused whatever the other copy holds (format spec 15).
    if let Some(found) = newer {
        return Err(newer_store(dir, found));
    }
    if let (Err(_), Err(_), Some(found)) = (&main, &copy, older) {
        return Err(PersistError::OlderFormat {
            path: dir.display().to_string(),
            found,
            supported: STORE_VERSION,
        });
    }
    Ok((main, copy))
}

/// Refuses a store of an older format ([`PersistError::OlderFormat`]) before an operation that
/// does not read `GRAPH` itself (listing snapshots, marks, attic entries, an export); anything
/// else about `GRAPH` is left to that operation.
pub(crate) fn refuse_older_format(dir: &DirPath) -> Result<(), PersistError> {
    refuse_other_version(dir)
}

/// The refusal of a store whose `GRAPH` is of a newer store format version `found`.
pub(crate) fn newer_store(dir: &DirPath, found: u32) -> PersistError {
    PersistError::UnsupportedVersion {
        location: format!("store {} (store format version)", dir.display()),
        found,
        supported: STORE_VERSION,
    }
}

/// The version guard of every operation on an existing store, backup or attic (format spec
/// 15), run BEFORE anything else of it is read and before any write or lock: refuses a newer
/// version ([`refuse_newer_version`]) and an older store format
/// ([`PersistError::OlderFormat`], as [`decode_graph_copies`] classifies it). What else is
/// wrong with `GRAPH`, and a header that cannot be read (a read failure, a damaged or short
/// header), is left to the operation, which reports it as before (damage reporting and
/// maintenance mode are unchanged).
pub(crate) fn refuse_other_version(dir: &DirPath) -> Result<(), PersistError> {
    refuse_newer_version(dir)?;
    decode_graph_copies(dir).map(|_| ())
}

/// The forward part of the version guard (format spec 15), also run by the upgrade converter,
/// which reads OLDER stores:
///
/// 1. a single-file container of a newer container version;
/// 2. `GRAPH` / `GRAPH.copy` of a newer store format version (either copy);
/// 3. any manifest, snapshot segment or WAL segment of the tree (snapshots, holders, `tmp-*`
///    directories, the attic) whose header is intact (magic and checksum) but whose file
///    format version is newer than this build's.
///
/// A newer version is [`PersistError::UnsupportedVersion`]: written by a newer Graphersal,
/// never damage, never repaired or upgraded. Reads only headers (64 bytes per segment, a
/// manifest's fixed part).
pub(crate) fn refuse_newer_version(dir: &DirPath) -> Result<(), PersistError> {
    refuse_newer_container(dir)?;
    let mut newer = None;
    for name in [GRAPH_FILE, GRAPH_COPY] {
        if let Ok(bytes) = read_bounded(&dir.join(name), 64 << 20) {
            newer = newer.max(GraphFile::newer_version(&bytes));
        }
    }
    if let Some(found) = newer {
        return Err(newer_store(dir, found));
    }
    // An intact GRAPH of this store format whose state word a newer version gave a value this
    // one does not know (format spec 5.1, 16): not damage, refused.
    for name in [GRAPH_FILE, GRAPH_COPY] {
        if let Ok(bytes) = read_bounded(&dir.join(name), 64 << 20)
            && let Some(state) = GraphFile::unknown_state(&bytes)
        {
            return Err(PersistError::unsupported_content(
                dir.join(name).display(),
                12,
                format!(
                    "the store state {state} is not known to this version (written by a newer \
                     Graphersal?)"
                ),
            ));
        }
    }
    refuse_newer_files(dir, dir, 0)
}

/// Step 1 of [`refuse_newer_version`]: the single-file container's preamble.
pub(crate) fn refuse_newer_container(dir: &DirPath) -> Result<(), PersistError> {
    #[cfg(not(target_arch = "wasm32"))]
    if dir.name().is_empty()
        && let Some(path) = dir.store_dir().local_path()
        && let Some(found) = super::super::single_file::newer_container_version(path)
    {
        return Err(PersistError::UnsupportedVersion {
            location: format!("single-file store {} (container version)", path.display()),
            found,
            supported: super::super::single_file::SUPPORTED_CONTAINER_VERSION,
        });
    }
    let _ = dir;
    Ok(())
}

/// Step 3 of [`refuse_newer_version`]: every file below `at` (depth-bounded: a store's tree
/// is at most `attic/<entry>/snapshots/<seq>/<file>` deep).
fn refuse_newer_files(root: &DirPath, at: &DirPath, depth: usize) -> Result<(), PersistError> {
    if depth > 6 {
        return Ok(());
    }
    // An unreadable listing is left to the operation (missing, damaged, not a directory).
    let Ok(entries) = at.list() else {
        return Ok(());
    };
    for (name, kind) in entries {
        let path = at.join(&name);
        // The embedding models (format spec 4.5) hold no file of the store's own formats.
        if depth == 0 && name == super::models::MODELS_DIR {
            continue;
        }
        match kind {
            EntryKind::Dir => refuse_newer_files(root, &path, depth + 1)?,
            EntryKind::File => match newer_file_version(&path) {
                Ok(None) => {}
                Ok(Some((what, found))) => {
                    return Err(PersistError::UnsupportedVersion {
                        location: format!(
                            "{what} {} of the store {} (file format version)",
                            path.relative_to(root),
                            root.display()
                        ),
                        found,
                        supported: super::super::codec::FORMAT_VERSION,
                    });
                }
                // An unreadable header is left to the operation's own checks.
                Err(_) => {}
            },
        }
    }
    Ok(())
}

/// The kind and file format version of a manifest, snapshot segment or WAL segment whose
/// header is intact but NEWER than this build reads; `Ok(None)` for everything else (another
/// file, a current or older version, a damaged or short header: damage is the operation's
/// business); `Err` for a header that could not be read.
fn newer_file_version(path: &DirPath) -> std::io::Result<Option<(&'static str, u32)>> {
    use super::super::codec::FORMAT_VERSION;
    use super::super::manifest::{FIXED_BYTES, MANIFEST_MAGIC, ManifestFixed};
    use super::super::segment::SEGMENT_MAGIC;
    use super::super::wal::WAL_MAGIC;
    const HEADER: usize = 64;
    let len = path.len()?;
    if len < HEADER as u64 {
        return Ok(None);
    }
    let head = path.read_range(0, HEADER)?;
    let magic = &head[..8];
    if magic == SEGMENT_MAGIC || magic == WAL_MAGIC {
        // Both headers: magic, version u32, ..., the CRC-32C of bytes 0..60 at 60.
        let stored = u32::from_le_bytes([head[60], head[61], head[62], head[63]]);
        let version = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
        let what = if magic == WAL_MAGIC {
            "WAL segment"
        } else {
            "snapshot segment"
        };
        return Ok(
            (crc(&head[..60]) == stored && version > FORMAT_VERSION).then_some((what, version))
        );
    }
    if magic == MANIFEST_MAGIC && len >= FIXED_BYTES as u64 {
        // The head copy of the fixed part, else the tail copy (format spec 6.1).
        // `Ok(Ok(()))`: an intact copy this build reads; `Ok(Err(version))`: not intact, or
        // intact and newer (`Some`).
        let copy = |at: u64| -> std::io::Result<Result<(), Option<u32>>> {
            let bytes = path.read_range(at, FIXED_BYTES)?;
            Ok(ManifestFixed::decode(&bytes)
                .map(|_| ())
                .map_err(|err| err.newer_version))
        };
        let found = match copy(0)? {
            Ok(()) => None,
            Err(Some(found)) => Some(found),
            Err(None) => copy(len - FIXED_BYTES as u64)?.err().flatten(),
        };
        return Ok(found.map(|found| ("manifest", found)));
    }
    Ok(None)
}

/// Writes `GRAPH.copy` and then `GRAPH` (each atomically: a temporary file, fsync, rename),
/// then syncs the directory.
pub(crate) fn write_graph_file(dir: &DirPath, file: &GraphFile) -> Result<(), PersistError> {
    let bytes = file.encode();
    write_atomic(dir, GRAPH_COPY, &bytes)?;
    write_atomic(dir, GRAPH_FILE, &bytes)
}

/// Writes `name` in `dir` atomically and durably ([`StoreDir::write_atomic`]).
pub(crate) fn write_atomic(dir: &DirPath, name: &str, bytes: &[u8]) -> Result<(), PersistError> {
    dir.join(name)
        .write_atomic(bytes)
        .map_err(|err| PersistError::io("writing a store file", err))
}

/// Syncs a directory, so a rename or a new file in it survives a crash.
pub(crate) fn sync_dir(dir: &DirPath) -> Result<(), PersistError> {
    dir.store_dir()
        .sync_dir(dir.name())
        .map_err(|err| PersistError::io("syncing a store directory", err))
}

/// Reads a whole file, refusing one larger than `max` bytes.
pub(crate) fn read_bounded(path: &DirPath, max: u64) -> std::io::Result<Vec<u8>> {
    path.store_dir().read(path.name(), max)
}

/// The directory name of the snapshot at `commit_seq`.
pub(crate) fn snapshot_name(commit_seq: u64) -> String {
    format!("{commit_seq:020}")
}

/// The file name of the WAL segment whose first commit is `first`.
pub(crate) fn wal_name(first: u64) -> String {
    format!("{first:020}.{WAL_EXTENSION}")
}

/// Parses a 20-digit name.
pub(crate) fn parse_seq(name: &str) -> Option<u64> {
    (name.len() == 20 && name.bytes().all(|b| b.is_ascii_digit()))
        .then(|| name.parse().ok())
        .flatten()
}

/// The snapshot directories of a store, oldest first: `(commit_seq, path)`.
pub(crate) fn list_snapshots(dir: &DirPath) -> Result<Vec<(u64, DirPath)>, PersistError> {
    let mut out = list_snapshot_dirs(dir)?;
    // A directory without a manifest only holds files newer snapshots use (format spec 4.1).
    out.retain(|(_, path)| path.join(super::snapshot_dir::MANIFEST_FILE).exists());
    Ok(out)
}

/// Every directory under `snapshots/` named by a commit, snapshots and holders of retained
/// files alike (format spec 4.1), in order.
pub(crate) fn list_snapshot_dirs(dir: &DirPath) -> Result<Vec<(u64, DirPath)>, PersistError> {
    let root = dir.join(SNAPSHOTS_DIR);
    let entries = root
        .list()
        .map_err(|err| PersistError::io("listing snapshots", err))?;
    let mut out: Vec<(u64, DirPath)> = entries
        .into_iter()
        .filter(|(_, kind)| *kind == EntryKind::Dir)
        .filter_map(|(name, _)| Some((parse_seq(&name)?, root.join(&name))))
        .collect();
    out.sort_by_key(|(seq, _)| *seq);
    Ok(out)
}

/// The directories under `snapshots/` without a manifest: holders of files that newer
/// snapshots reference (format spec 4.1, 12.4), in order.
pub(crate) fn list_holders(dir: &DirPath) -> Result<Vec<(u64, DirPath)>, PersistError> {
    let mut out = list_snapshot_dirs(dir)?;
    out.retain(|(_, path)| !path.join(super::snapshot_dir::MANIFEST_FILE).exists());
    Ok(out)
}

/// The WAL segment files of a store, in order: `(first commit_seq, path)`.
pub(crate) fn list_wal(dir: &DirPath) -> Result<Vec<(u64, DirPath)>, PersistError> {
    let root = dir.join(WAL_DIR);
    let entries = root
        .list()
        .map_err(|err| PersistError::io("listing the WAL", err))?;
    let suffix = format!(".{WAL_EXTENSION}");
    let mut out: Vec<(u64, DirPath)> = entries
        .into_iter()
        .filter(|(_, kind)| *kind == EntryKind::File)
        .filter_map(|(name, _)| {
            let seq = parse_seq(name.strip_suffix(&suffix)?)?;
            Some((seq, root.join(&name)))
        })
        .collect();
    out.sort_by_key(|(seq, _)| *seq);
    Ok(out)
}

/// The WAL segments a replay from a snapshot at `snapshot_seq` needs: the last one that may
/// hold commit `snapshot_seq + 1` (its first commit is at or before it) and every later one.
pub(crate) fn wal_after(segments: &[(u64, DirPath)], snapshot_seq: u64) -> &[(u64, DirPath)] {
    let next = snapshot_seq.saturating_add(1);
    let start = segments
        .iter()
        .rposition(|(first, _)| *first <= next)
        .unwrap_or(0);
    &segments[start..]
}

/// The writer lock of a store directory ([`LockKind::Writer`]), held for the life of a
/// writer.
pub(crate) struct DirLock {
    _lock: DirLockGuard,
}

impl std::fmt::Debug for DirLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirLock")
    }
}

impl DirLock {
    /// Takes the writer lock without waiting.
    pub(crate) fn exclusive(dir: &DirPath) -> Result<Self, PersistError> {
        match dir.store_dir().lock(LockKind::Writer) {
            Ok(Some(lock)) => Ok(Self { _lock: lock }),
            Ok(None) => Err(PersistError::Locked {
                path: dir.display(),
            }),
            Err(err) => Err(PersistError::io("opening the store's LOCK file", err)),
        }
    }
}

/// The backup pin: shared while a backup copies files (any number of backups, in this or
/// another process), exclusive while `prune` deletes files.
pub(crate) struct BackupPin {
    _lock: DirLockGuard,
}

impl BackupPin {
    /// Pins the store's files for a backup (waits while a prune deletes files).
    pub(crate) fn shared(dir: &DirPath) -> Result<Self, PersistError> {
        match dir.store_dir().lock(LockKind::BackupShared) {
            Ok(Some(lock)) => Ok(Self { _lock: lock }),
            Ok(None) => Err(PersistError::store(
                "back up the store",
                StoreFailure::PruneRunning,
                "a prune is deleting its files; back up again when it has finished",
            )),
            Err(err) => Err(PersistError::io("pinning the store for a backup", err)),
        }
    }

    /// The prune side: fails at once when a backup holds the pin.
    pub(crate) fn exclusive(dir: &DirPath) -> Result<Self, PersistError> {
        match dir.store_dir().lock(LockKind::BackupExclusive) {
            Ok(Some(lock)) => Ok(Self { _lock: lock }),
            Ok(None) => Err(PersistError::store(
                "prune the store",
                StoreFailure::BackupRunning,
                "a backup is copying its files; prune again when it has finished",
            )),
            Err(err) => Err(PersistError::io("opening the store's BACKUP file", err)),
        }
    }
}

/// The `INTENT` file (format spec section 11): a multi-file operation in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Intent {
    pub(crate) op: String,
    /// Paths relative to the store directory.
    pub(crate) files: Vec<String>,
    pub(crate) started_at: i64,
    /// The operation's other parameters (string values; format spec 11.1).
    pub(crate) params: Vec<(String, String)>,
}

impl Intent {
    fn to_json(&self) -> String {
        let mut object = PropertyMap::default();
        object.insert("op", ElementProperty::from(self.op.as_str()));
        object.insert(
            "files",
            ElementProperty::Array(
                self.files
                    .iter()
                    .map(|f| ElementProperty::from(f.as_str()))
                    .collect(),
            ),
        );
        object.insert("started_at", ElementProperty::from(self.started_at));
        for (key, value) in &self.params {
            object.insert(key.as_str(), ElementProperty::from(value.as_str()));
        }
        physical::to_json_text(&ElementProperty::Object(Box::new(object))).unwrap_or_default()
    }

    fn from_json(text: &str) -> Result<Self, String> {
        let value =
            physical::lift_untyped(physical::read_json(text, 8).map_err(|e| e.to_string())?);
        let ElementProperty::Object(object) = value else {
            return Err("not a JSON object".into());
        };
        let op = match object.get("op") {
            Some(ElementProperty::String(op)) => op.to_string(),
            _ => return Err("no \"op\"".into()),
        };
        let mut files = Vec::new();
        if let Some(ElementProperty::Array(items)) = object.get("files") {
            for item in items {
                match item {
                    ElementProperty::String(path) => files.push(path.to_string()),
                    _ => return Err("a file that is not a string".into()),
                }
            }
        }
        let started_at = match object.get("started_at") {
            Some(ElementProperty::Number(n)) => n.as_i64().unwrap_or(0),
            _ => 0,
        };
        let mut params = Vec::new();
        for (key, value) in object.iter() {
            if let ElementProperty::String(value) = value
                && key.as_str() != "op"
            {
                params.push((key.to_string(), value.to_string()));
            }
        }
        Ok(Self {
            op,
            files,
            started_at,
            params,
        })
    }

    /// The parameter `key` (`None`: missing).
    pub(crate) fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Writes the intent durably before the first file is touched.
    pub(crate) fn write(&self, dir: &DirPath) -> Result<(), PersistError> {
        write_atomic(dir, INTENT_FILE, self.to_json().as_bytes())
    }

    /// Reads a pending intent (`None`: no operation was interrupted).
    pub(crate) fn read(dir: &DirPath) -> Result<Option<Self>, PersistError> {
        let path = dir.join(INTENT_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_bounded(&path, 64 << 20)
            .map_err(|err| PersistError::io("reading the INTENT file", err))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| PersistError::corrupt(INTENT_FILE, 0, "the INTENT file is not UTF-8"))?;
        Self::from_json(&text)
            .map(Some)
            .map_err(|reason| PersistError::corrupt(INTENT_FILE, 0, reason))
    }

    /// Removes the intent once the operation is complete.
    pub(crate) fn clear(dir: &DirPath) -> Result<(), PersistError> {
        match dir.join(INTENT_FILE).remove_file() {
            Ok(()) => sync_dir(dir),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(PersistError::io("removing the INTENT file", err)),
        }
    }
}

/// A store-relative path from an intent or a manifest, checked: relative, no `..`, no root.
pub(crate) fn safe_relative(dir: &DirPath, relative: &str) -> Option<DirPath> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.contains('\\')
        || relative.contains('\0')
        || relative.contains(':')
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(dir.join(relative))
}

/// Removes a file or a directory tree; a missing one is fine (idempotent steps).
pub(crate) fn remove_path(path: &DirPath) -> std::io::Result<()> {
    path.remove()
}

/// Whether directory `dir` holds anything (a missing directory holds nothing).
pub(crate) fn has_entries(dir: &DirPath) -> bool {
    dir.exists() && dir.list().map(|e| !e.is_empty()).unwrap_or(true)
}

/// Removes what a failed create, copy or restore left in `dir`: its contents, and the
/// directory itself when it did not exist before.
pub(crate) fn undo_new_dir(dir: &DirPath, existed: bool) {
    if let Ok(entries) = dir.list() {
        for (name, _) in entries {
            let _ = dir.join(&name).remove();
        }
    }
    if !existed {
        let _ = dir.remove();
    }
}

/// The store format version this build writes and reads (format spec 5.1, 15): the version
/// word of `GRAPH`. Version 2 added the store id (offset 100); version 3 (Graphersal 0.2.0)
/// has the same `GRAPH` layout and marks a store whose snapshots and WAL are of file format
/// version 2 (the temporal value tags), so that a build that reads only file format version 1
/// refuses the store as newer instead of taking its files for damage. A store of version 1 or
/// 2 is refused with [`PersistError::OlderFormat`]; version 2 is upgraded by the converter
/// (`upgrade.rs`, format spec 15.1), version 1 (never released) migrates through a packed
/// snapshot.
pub(crate) const STORE_VERSION: u32 = 3;
