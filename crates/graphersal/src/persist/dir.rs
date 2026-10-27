//! The directory a [`Store`](super::Store) keeps its files in, as a trait (**EXPERIMENTAL in
//! 0.2.x**, like `GraphStorage`): [`StoreDir`], with [`FsDir`] (a directory of the file system,
//! native only) and [`MemDir`] (in memory, every target including wasm32).
//!
//! # Implementer's guide
//!
//! A Store names its files with **logical, relative names**: `GRAPH`, `wal/00000000000000000013.wal`,
//! `snapshots/00000000000000000012/manifest`, `attic/<id>/wal/...`. Parts are separated by `/`;
//! a name never starts with `/`, never contains an empty part, `.`, `..`, `\`, `:` or NUL (the
//! Store checks names it reads from its own files before using them). The empty name `""` is
//! the root. Nothing assumes that two names are two OS files: a backend may keep every name in
//! one container file, in a database, or in memory.
//!
//! "Directories" are namespaces: [`StoreDir::create_dir_all`] makes one exist,
//! [`StoreDir::list`] lists its direct children, [`StoreDir::rename`] and
//! [`StoreDir::remove_tree`] act on a whole subtree. Creating a file in a directory that does
//! not exist fails with [`std::io::ErrorKind::NotFound`] (as on a file system; the Store creates
//! its directories first).
//!
//! **Atomicity and durability**, per method:
//! - [`StoreDir::write_atomic`]: after it returns, a crash leaves either the old or the new
//!   content of the name, never a mix, and the new content is durable.
//! - [`StoreDir::rename`]: atomic (the source and the target name never both or neither exist
//!   after a crash); durable after [`StoreDir::sync_dir`] of the parent(s).
//! - Writes through a [`StoreFile`] are durable after [`StoreFile::sync`]; a crash may keep any
//!   prefix of unsynced appended bytes (the WAL is built for exactly that: a torn tail is cut).
//! - [`StoreDir::truncate`] is durable when it returns.
//! - A new name (a created file, directory, a rename, a removal) is durable after
//!   [`StoreDir::sync_dir`] of its parent. A backend without that notion makes `sync_dir` a
//!   no-op and every name change durable at once (or never, like [`MemDir`]).
//!
//! **Writes are appends.** A [`StoreFile`] is only ever written at its end: a new file from 0,
//! [`StoreDir::open_write`] at the file's current length, after [`StoreFile::set_len`] at the
//! new (shorter) length. No byte inside a file is ever overwritten in place; a file's whole
//! content changes only through [`StoreDir::write_atomic`] (a replacement). Lengths and offsets
//! ([`StoreDir::file_len`], [`StoreDir::read_range`], `set_len`, `truncate`) are LOGICAL bytes
//! (what was written), never the bytes a backend stores (an encrypting or compressing backend
//! answers in logical bytes). Removals ([`StoreDir::remove_file`], [`StoreDir::remove_tree`])
//! may be logical: a backend need not give the space back at once.
//!
//! **Backends.** [`FsDir`] (a directory), [`MemDir`] (memory) and, native only,
//! [`SingleFileDir`](super::SingleFileDir): the whole store in ONE log-structured file (appended
//! records, a double header with generation and CRC pointing at a name -> extents table,
//! removal is logical). A backend that keeps garbage reports it through
//! [`StoreDir::space_usage`] and gives it back in [`StoreDir::compact`]; [`StoreDir::check`]
//! reports damage of a backend's own metadata. These capability methods have default
//! implementations (later ones will too). **Anticipated**: a transparent encryption decorator
//! (`EncryptedDir<D>` over any backend: AEAD per block or per appended record): the trait is
//! object-safe (a wrapper holds a `Box<dyn StoreDir>` or a generic inner and forwards the
//! capability methods), appends and replacements are distinct operations, and lengths are
//! logical.
//!
//! **Locks** ([`StoreDir::lock`]): the writer lock is exclusive and never waits (a second open
//! fails with `PersistError::Locked`); the backup pin is shared (backups, waits while a prune
//! holds it) or exclusive (prune, never waits). A backend shared between processes must make
//! them work between processes (the file system's `flock`), an in-process one between threads.
//! A lock is held as long as its [`DirLockGuard`] lives.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};

/// What a name of a [`StoreDir`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntryKind {
    /// A file (bytes).
    File,
    /// A directory (a namespace of further names).
    Dir,
}

/// One child of a directory ([`StoreDir::list`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirEntry {
    /// The child's own name (one part, no `/`).
    pub name: String,
    /// File or directory.
    pub kind: EntryKind,
}

impl DirEntry {
    /// An entry (for implementers of [`StoreDir::list`]).
    pub fn new(name: impl Into<String>, kind: EntryKind) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }
}

/// The locks of a store directory ([`StoreDir::lock`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LockKind {
    /// The one writer (an open read-write `Store`): exclusive, never waits.
    Writer,
    /// A backup pins the files (shared; waits while a prune holds the exclusive pin).
    BackupShared,
    /// A prune deletes files (exclusive; never waits: `None` while a backup holds the pin).
    BackupExclusive,
}

/// A held lock of a [`StoreDir`]: released when dropped.
pub struct DirLockGuard(#[allow(dead_code)] Box<dyn std::any::Any + Send + Sync>);

impl DirLockGuard {
    /// Wraps whatever keeps the lock held (an open locked file, an in-process token).
    pub fn new(holder: impl std::any::Any + Send + Sync) -> Self {
        Self(Box::new(holder))
    }
}

impl std::fmt::Debug for DirLockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirLockGuard")
    }
}

/// A file of a [`StoreDir`] open for writing (the WAL segment the journal appends to).
pub trait StoreFile: Write + Send + Sync {
    /// Makes everything written so far durable (`fdatasync`; a no-op in memory).
    fn sync(&mut self) -> io::Result<()>;

    /// Cuts the file to `len` bytes (at most its length; the Store only shrinks) and makes
    /// that durable; the next write goes to `len`.
    fn set_len(&mut self, len: u64) -> io::Result<()>;
}

/// The directory a Store keeps its files in. **EXPERIMENTAL in 0.2.x**; see the module
/// documentation for names, atomicity and locks.
pub trait StoreDir: Send + Sync + std::fmt::Debug {
    /// Where the directory is, for diagnostics (a path, `memory:main`).
    fn location(&self) -> String;

    /// Whether the contents are gone with the process (in memory; default `false`). Front
    /// ends say so ("lost on reload").
    fn is_volatile(&self) -> bool {
        false
    }

    /// The file system path, when the directory is one (default `None`).
    #[cfg(not(target_arch = "wasm32"))]
    fn local_path(&self) -> Option<&std::path::Path> {
        None
    }

    /// What `name` is: `None` when it does not exist.
    fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>>;

    /// The direct children of directory `dir` (any order). A missing directory is
    /// [`io::ErrorKind::NotFound`].
    fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>>;

    /// The size of file `name` in bytes.
    fn file_len(&self, name: &str) -> io::Result<u64>;

    /// The whole file `name`; a file larger than `max` bytes is [`io::ErrorKind::InvalidData`]
    /// (nothing larger is buffered).
    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>>;

    /// `len` bytes of file `name` from `offset`; fewer available is
    /// [`io::ErrorKind::UnexpectedEof`].
    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>>;

    /// A sequential reader of file `name` (snapshot segments, WAL segments).
    fn open_read(&self, name: &str) -> io::Result<Box<dyn Read + Send>>;

    /// Makes directory `name` and its parents exist (existing ones are fine).
    fn create_dir_all(&self, name: &str) -> io::Result<()>;

    /// Creates file `name` for writing: `exclusive` fails with
    /// [`io::ErrorKind::AlreadyExists`] when it exists, otherwise an existing file is emptied.
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>>;

    /// Opens the existing file `name` for appending at `offset`, which is its current length
    /// (the WAL continues there).
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>>;

    /// Appends `bytes` to file `name` (created when missing) and makes them durable.
    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()>;

    /// Replaces file `name` atomically and durably by `bytes` (see the module documentation).
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()>;

    /// Moves a file or a whole directory tree to another name. A file's target is replaced.
    /// The target appears whole or not at all; within one device the move is atomic. A
    /// backend that has to copy (directories on different devices) copies into a temporary
    /// name in the target's directory, makes it durable, renames it there and only then
    /// removes the source: a crash in between may leave BOTH names, and repeating the call
    /// completes the move (a directory target that exists while the source does too is the
    /// complete copy: only the source is removed). The Store runs every move under an `INTENT`
    /// and repeats it at the next open.
    fn rename(&self, from: &str, to: &str) -> io::Result<()>;

    /// Cuts file `name` to `len` bytes (at most its length), durably.
    fn truncate(&self, name: &str, len: u64) -> io::Result<()>;

    /// Removes file `name` ([`io::ErrorKind::NotFound`] when it does not exist).
    fn remove_file(&self, name: &str) -> io::Result<()>;

    /// Removes a file or a whole directory tree; a missing name is fine. A name that a
    /// backend stores elsewhere (a symbolic link of [`FsDir`]) is removed with its contents.
    fn remove_tree(&self, name: &str) -> io::Result<()>;

    /// Makes the names in directory `name` durable (new files, renames, removals). A no-op
    /// where that is meaningless.
    fn sync_dir(&self, name: &str) -> io::Result<()>;

    /// Takes a lock of the directory: `Ok(None)` when a [`LockKind::Writer`] or
    /// [`LockKind::BackupExclusive`] lock is held elsewhere (they never wait).
    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>>;

    /// The physical space of a backend that keeps garbage (a single-file container, where a
    /// removal only frees space logically): `None` (the default) for a backend whose removals
    /// give the space back at once ([`FsDir`], [`MemDir`]). Must be cheap (bookkeeping only,
    /// no data reads): it is polled. `Err`: the backend cannot read its bookkeeping now (an
    /// I/O error, a poisoned lock); the Store operation that asked fails with it.
    fn space_usage(&self) -> io::Result<Option<SpaceUsage>> {
        Ok(None)
    }

    /// Gives the garbage back ([`StoreDir::space_usage`]): rewrites the live data compactly.
    /// `Ok(None)` (the default): nothing to do for this backend.
    fn compact(&self) -> io::Result<Option<DirCompaction>> {
        Ok(None)
    }

    /// The free space of the file system the directory lives on, in bytes (`Ok(None)`, the
    /// default: not applicable, for example memory). `Err`: the backend asked the file system
    /// and the query failed (`statvfs`); the Store operation that consults it fails with it,
    /// it never reads a failure as "unknown".
    fn free_space(&self) -> io::Result<Option<u64>> {
        Ok(None)
    }

    /// What the backend found about its OWN integrity when it opened (a container's tables and
    /// records, before any store file is read): damage makes the Store open in maintenance
    /// mode, warnings are reported. Empty (the default) for backends without own metadata.
    /// `Err`: the backend cannot check itself (an I/O error, a poisoned lock): the Store records
    /// that as damage of the backend ("the backend could not check itself: ...") and opens in
    /// maintenance mode, as for any other damage.
    fn check(&self) -> io::Result<DirCheck> {
        Ok(DirCheck::default())
    }
}

/// [`StoreDir::check`] as the Store reads it: a backend that cannot check itself is damaged
/// (the Store opens in maintenance mode and names the error), never "found nothing".
pub(crate) fn backend_check(dir: &dyn StoreDir) -> DirCheck {
    dir.check().unwrap_or_else(|err| DirCheck {
        damage: vec![format!("the backend could not check itself: {err}")],
        warnings: Vec::new(),
    })
}

/// The physical space of a store directory ([`StoreDir::space_usage`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SpaceUsage {
    /// Bytes the backend occupies (the container file's size).
    pub total_bytes: u64,
    /// Bytes a compaction would keep (the live data plus its framing, estimated from the
    /// backend's bookkeeping).
    pub live_bytes: u64,
}

impl SpaceUsage {
    /// A usage of `total_bytes`, of which `live_bytes` are live.
    pub fn new(total_bytes: u64, live_bytes: u64) -> Self {
        Self {
            total_bytes,
            live_bytes,
        }
    }

    /// Bytes a compaction would give back (`total - live`, at least 0).
    pub fn garbage_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.live_bytes)
    }

    /// The garbage as a share of the total (0.0 for an empty backend).
    pub fn garbage_ratio(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.garbage_bytes() as f64 / self.total_bytes as f64
        }
    }
}

/// What a backend compaction did ([`StoreDir::compact`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DirCompaction {
    /// The backend's size before.
    pub bytes_before: u64,
    /// The backend's size after.
    pub bytes_after: u64,
}

impl DirCompaction {
    /// A compaction from `bytes_before` to `bytes_after`.
    pub fn new(bytes_before: u64, bytes_after: u64) -> Self {
        Self {
            bytes_before,
            bytes_after,
        }
    }
}

/// A backend's own integrity ([`StoreDir::check`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DirCheck {
    /// Damage of the backend's own metadata or records (each one line): the Store opens in
    /// maintenance mode; the backend refuses writes.
    pub damage: Vec<String>,
    /// Problems that lost nothing (a redundant copy was used).
    pub warnings: Vec<String>,
}

impl DirCheck {
    /// Whether nothing was found.
    pub fn is_ok(&self) -> bool {
        self.damage.is_empty() && self.warnings.is_empty()
    }
}

/// Checks a store name (see the module documentation): relative, `/`-separated, no empty,
/// `.` or `..` part, no `\`, `:` or NUL. The empty name is the root. Used by the native
/// backends only (`FsDir`, `SingleFileDir`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn check_name(name: &str) -> io::Result<()> {
    if name.is_empty() {
        return Ok(());
    }
    if name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains(':')
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a store name: {name:?}"),
        ));
    }
    Ok(())
}

/// Converts what names a store directory into the directory: a path (a [`FsDir`], native
/// only), a [`MemDir`], a [`FsDir`] or any shared [`StoreDir`].
pub trait IntoStoreDir {
    /// The directory.
    fn into_store_dir(self) -> Arc<dyn StoreDir>;
}

impl IntoStoreDir for Arc<dyn StoreDir> {
    fn into_store_dir(self) -> Arc<dyn StoreDir> {
        self
    }
}

impl IntoStoreDir for &Arc<dyn StoreDir> {
    fn into_store_dir(self) -> Arc<dyn StoreDir> {
        self.clone()
    }
}

impl IntoStoreDir for MemDir {
    fn into_store_dir(self) -> Arc<dyn StoreDir> {
        Arc::new(self)
    }
}

impl IntoStoreDir for &MemDir {
    fn into_store_dir(self) -> Arc<dyn StoreDir> {
        Arc::new(self.clone())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod fs_dir {
    use super::*;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Seek, SeekFrom};
    use std::path::{Path, PathBuf};

    /// A [`StoreDir`] on the file system (native only): every name is a file or directory below
    /// the root, files are synced with `fsync`, directories after a change of names, locks are
    /// OS locks on the `LOCK` and `BACKUP` files (`flock` / `LockFileEx`, released by the OS
    /// when the process ends, also on a crash).
    ///
    /// **Relocatable.** Nothing in a store's files names a path (only names relative to the
    /// root), so the directory can be moved or copied anywhere and opened there. Parts of it
    /// (`wal/`, `snapshots/`, `attic/`, a single snapshot directory) may be SYMBOLIC LINKS to
    /// other disks: `FsDir` never resolves or canonicalizes links and never records their
    /// targets. A move between two such parts that live on different file systems (a
    /// rollback moves WAL segments and snapshots into the attic, a restore moves them back)
    /// falls back to copy, sync, rename in the target directory, remove the source
    /// ([`StoreDir::rename`]; repeated at the next open after a crash). Removing a name that is
    /// a link to a directory (prune, attic removal) removes the link AND the contents it
    /// points at (the linked directory itself stays, empty). A link that is itself moved (a
    /// single linked snapshot directory going into the attic) is moved as a link: give it an
    /// absolute target.
    #[derive(Debug, Clone)]
    pub struct FsDir {
        root: PathBuf,
    }

    impl FsDir {
        /// The directory at `root` (it need not exist yet: `Store::create` makes it).
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self { root: root.into() }
        }

        /// The root path.
        pub fn path(&self) -> &Path {
            &self.root
        }

        fn at(&self, name: &str) -> io::Result<PathBuf> {
            if name.is_empty() {
                return Ok(self.root.clone());
            }
            check_name(name)?;
            Ok(self.root.join(name))
        }

        fn lock_file(&self, name: &str) -> io::Result<fd_lock::RwLock<File>> {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(self.at(name)?)?;
            Ok(fd_lock::RwLock::new(file))
        }
    }

    struct FsFile(File);

    impl Write for FsFile {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    impl StoreFile for FsFile {
        fn sync(&mut self) -> io::Result<()> {
            self.0.sync_data()
        }

        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.0.set_len(len)?;
            self.0.seek(SeekFrom::Start(len))?;
            self.0.sync_all()
        }
    }

    /// Keeps an OS lock held: the locked file stays open (the guard only borrows it).
    struct HeldLock(#[allow(dead_code)] fd_lock::RwLock<File>);

    impl StoreDir for FsDir {
        fn location(&self) -> String {
            self.root.display().to_string()
        }

        fn local_path(&self) -> Option<&Path> {
            Some(&self.root)
        }

        fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>> {
            match fs::metadata(self.at(name)?) {
                Ok(meta) if meta.is_dir() => Ok(Some(EntryKind::Dir)),
                Ok(_) => Ok(Some(EntryKind::File)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(err),
            }
        }

        fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>> {
            let mut out = Vec::new();
            for entry in fs::read_dir(self.at(dir)?)? {
                let entry = entry?;
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let kind = if entry.path().is_dir() {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                };
                out.push(DirEntry { name, kind });
            }
            Ok(out)
        }

        fn file_len(&self, name: &str) -> io::Result<u64> {
            Ok(fs::metadata(self.at(name)?)?.len())
        }

        fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
            let file = File::open(self.at(name)?)?;
            let mut bytes = Vec::new();
            file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > max {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} is larger than {max} bytes"),
                ));
            }
            Ok(bytes)
        }

        fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
            let mut file = File::open(self.at(name)?)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0u8; len];
            file.read_exact(&mut bytes)?;
            Ok(bytes)
        }

        fn open_read(&self, name: &str) -> io::Result<Box<dyn Read + Send>> {
            Ok(Box::new(io::BufReader::with_capacity(
                1 << 16,
                File::open(self.at(name)?)?,
            )))
        }

        fn create_dir_all(&self, name: &str) -> io::Result<()> {
            fs::create_dir_all(self.at(name)?)
        }

        fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
            let path = self.at(name)?;
            let file = if exclusive {
                OpenOptions::new().create_new(true).write(true).open(path)?
            } else {
                File::create(path)?
            };
            Ok(Box::new(FsFile(file)))
        }

        fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
            let mut file = OpenOptions::new().write(true).open(self.at(name)?)?;
            file.seek(SeekFrom::Start(offset))?;
            Ok(Box::new(FsFile(file)))
        }

        fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.at(name)?)?;
            file.write_all(bytes)?;
            file.sync_data()
        }

        fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
            let path = self.at(name)?;
            let tmp = self.at(&format!("{name}.tmp"))?;
            let result = File::create(&tmp)
                .and_then(|mut file| {
                    file.write_all(bytes)?;
                    file.sync_all()
                })
                .and_then(|()| fs::rename(&tmp, &path));
            if let Err(err) = result {
                let _ = fs::remove_file(&tmp);
                return Err(err);
            }
            let parent = name.rsplit_once('/').map_or("", |(dir, _)| dir);
            self.sync_dir(parent)
        }

        fn rename(&self, from: &str, to: &str) -> io::Result<()> {
            let (source, target) = (self.at(from)?, self.at(to)?);
            // A cross-device move interrupted after its copy: the target is complete.
            if target.is_dir() && source.is_dir() {
                remove_all(&source)?;
                return sync_parent(&source);
            }
            match fs::rename(&source, &target) {
                Err(err) if is_cross_device(&err) => move_across_devices(&source, &target),
                other => other,
            }
        }

        fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
            let file = OpenOptions::new().write(true).open(self.at(name)?)?;
            file.set_len(len)?;
            file.sync_all()
        }

        fn remove_file(&self, name: &str) -> io::Result<()> {
            fs::remove_file(self.at(name)?)
        }

        fn remove_tree(&self, name: &str) -> io::Result<()> {
            match remove_all(&self.at(name)?) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }

        fn sync_dir(&self, name: &str) -> io::Result<()> {
            #[cfg(unix)]
            {
                File::open(self.at(name)?).and_then(|dir| dir.sync_all())
            }
            #[cfg(not(unix))]
            {
                let _ = name;
                Ok(())
            }
        }

        fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
            match kind {
                LockKind::Writer | LockKind::BackupExclusive => {
                    let name = if kind == LockKind::Writer {
                        "LOCK"
                    } else {
                        "BACKUP"
                    };
                    let mut lock = self.lock_file(name)?;
                    match lock.try_write() {
                        // The lock lives as long as the file stays open.
                        Ok(guard) => std::mem::forget(guard),
                        Err(_) => return Ok(None),
                    }
                    Ok(Some(DirLockGuard::new(HeldLock(lock))))
                }
                LockKind::BackupShared => {
                    let lock = self.lock_file("BACKUP")?;
                    std::mem::forget(lock.read()?);
                    Ok(Some(DirLockGuard::new(HeldLock(lock))))
                }
            }
        }
    }

    /// Whether a rename failed because source and target are on different file systems.
    fn is_cross_device(err: &io::Error) -> bool {
        if err.kind() == io::ErrorKind::CrossesDevices {
            return true;
        }
        // EXDEV on Unix, ERROR_NOT_SAME_DEVICE on Windows.
        #[cfg(unix)]
        let code = 18;
        #[cfg(windows)]
        let code = 17;
        #[cfg(not(any(unix, windows)))]
        let code = -1;
        err.raw_os_error() == Some(code)
    }

    /// Syncs the directory holding `path`.
    fn sync_parent(path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    /// Removes a file or a directory tree. A symbolic link to a directory (a part of the store
    /// placed on another disk) is removed together with the contents it points at, never left
    /// dangling with them; the link target itself (a directory elsewhere) is emptied, not
    /// removed.
    fn remove_all(path: &Path) -> io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            if path.is_dir() {
                for entry in fs::read_dir(path)? {
                    remove_all(&entry?.path())?;
                }
            }
            return fs::remove_file(path);
        }
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                remove_all(&entry?.path())?;
            }
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        }
    }

    /// Copies a file or a tree (following links) to `target`, every file synced.
    fn copy_all(source: &Path, target: &Path) -> io::Result<()> {
        if source.is_dir() {
            fs::create_dir(target)?;
            for entry in fs::read_dir(source)? {
                let entry = entry?;
                copy_all(&entry.path(), &target.join(entry.file_name()))?;
            }
            #[cfg(unix)]
            File::open(target)?.sync_all()?;
            Ok(())
        } else {
            fs::copy(source, target)?;
            // Opened for writing: Windows refuses to flush a read-only handle (access denied).
            fs::OpenOptions::new().write(true).open(target)?.sync_all()
        }
    }

    /// The move of [`StoreDir::rename`] between file systems: copy into `tmp-xdev-<name>` next
    /// to the target (a name the Store's listings ignore and its open cleans), sync, rename
    /// there, sync the target's directory, then remove the source and sync its directory.
    fn move_across_devices(source: &Path, target: &Path) -> io::Result<()> {
        let name = target
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no target name"))?;
        let tmp = target.with_file_name(format!("tmp-xdev-{}", name.to_string_lossy()));
        if fs::symlink_metadata(&tmp).is_ok() {
            remove_all(&tmp)?;
        }
        copy_all(source, &tmp)?;
        fs::rename(&tmp, target)?;
        sync_parent(target)?;
        remove_all(source)?;
        sync_parent(source)
    }

    impl IntoStoreDir for FsDir {
        fn into_store_dir(self) -> Arc<dyn StoreDir> {
            Arc::new(self)
        }
    }

    /// The backend a path names: an existing regular file, or a missing path with the
    /// extension `.gstore`, is a single-file store
    /// ([`SingleFileDir`](crate::persist::SingleFileDir)); anything else is a directory
    /// ([`FsDir`]). Every `impl IntoStoreDir` path argument goes through this.
    pub fn store_dir_at(path: impl AsRef<Path>) -> Arc<dyn StoreDir> {
        let path = path.as_ref();
        if crate::persist::single_file::names_single_file(path) {
            Arc::new(crate::persist::SingleFileDir::new(path))
        } else {
            Arc::new(FsDir::new(path))
        }
    }

    impl IntoStoreDir for &Path {
        fn into_store_dir(self) -> Arc<dyn StoreDir> {
            store_dir_at(self)
        }
    }

    impl IntoStoreDir for PathBuf {
        fn into_store_dir(self) -> Arc<dyn StoreDir> {
            store_dir_at(self)
        }
    }

    impl IntoStoreDir for &PathBuf {
        fn into_store_dir(self) -> Arc<dyn StoreDir> {
            store_dir_at(self)
        }
    }

    impl IntoStoreDir for &str {
        fn into_store_dir(self) -> Arc<dyn StoreDir> {
            store_dir_at(self)
        }
    }

    impl IntoStoreDir for String {
        fn into_store_dir(self) -> Arc<dyn StoreDir> {
            store_dir_at(self)
        }
    }

    impl IntoStoreDir for &String {
        fn into_store_dir(self) -> Arc<dyn StoreDir> {
            store_dir_at(self.as_str())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn temp(name: &str) -> PathBuf {
            let path = std::env::temp_dir()
                .join(format!("graphersal-fsdir-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            path
        }

        /// The copy fallback of a cross-device move (exercised on one device here), and its
        /// completion after a crash between the copy and the removal of the source.
        #[test]
        fn a_move_across_devices_copies_then_removes_and_completes_after_a_crash() {
            let root = temp("xdev");
            let dir = FsDir::new(&root);
            dir.create_dir_all("snapshots/00000000000000000003")
                .unwrap();
            dir.write_atomic("snapshots/00000000000000000003/manifest", b"m")
                .unwrap();
            dir.create_dir_all("attic/a/snapshots").unwrap();
            move_across_devices(
                &root.join("snapshots/00000000000000000003"),
                &root.join("attic/a/snapshots/00000000000000000003"),
            )
            .unwrap();
            assert_eq!(
                dir.entry_kind("snapshots/00000000000000000003").unwrap(),
                None
            );
            assert_eq!(
                dir.read("attic/a/snapshots/00000000000000000003/manifest", 9)
                    .unwrap(),
                b"m"
            );
            // A crash after the copy: both exist; the repeated move removes the source.
            copy_all(
                &root.join("attic/a/snapshots/00000000000000000003"),
                &root.join("snapshots/00000000000000000003"),
            )
            .unwrap();
            dir.rename(
                "snapshots/00000000000000000003",
                "attic/a/snapshots/00000000000000000003",
            )
            .unwrap();
            assert_eq!(
                dir.entry_kind("snapshots/00000000000000000003").unwrap(),
                None
            );
            assert!(is_cross_device(&io::Error::from(
                io::ErrorKind::CrossesDevices
            )));
            let _ = fs::remove_dir_all(&root);
        }

        #[cfg(unix)]
        #[test]
        fn removing_a_linked_directory_removes_its_contents_and_the_link() {
            let root = temp("link-root");
            let elsewhere = temp("link-target");
            fs::write(elsewhere.join("seg"), b"x").unwrap();
            std::os::unix::fs::symlink(&elsewhere, root.join("snap")).unwrap();
            let dir = FsDir::new(&root);
            assert_eq!(dir.entry_kind("snap").unwrap(), Some(EntryKind::Dir));
            dir.remove_tree("snap").unwrap();
            assert!(fs::symlink_metadata(root.join("snap")).is_err());
            assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
            let _ = fs::remove_dir_all(&root);
            let _ = fs::remove_dir_all(&elsewhere);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use fs_dir::{FsDir, store_dir_at};

/// What a [`MemDir`] holds.
#[derive(Debug, Default, Clone)]
struct MemState {
    files: BTreeMap<String, Arc<Vec<u8>>>,
    dirs: BTreeSet<String>,
    writer: bool,
    backups: usize,
    pruning: bool,
}

#[derive(Debug, Default)]
struct MemInner {
    name: String,
    state: Mutex<MemState>,
    released: Condvar,
}

/// A [`StoreDir`] in memory (every target, also wasm32): a map of names to bytes. Clones share
/// the same directory (a store open on it, read-only loads and listings of it). Syncs are
/// no-ops, the locks are in-process flags. Everything is gone when the last clone is dropped.
#[derive(Debug, Clone)]
pub struct MemDir {
    inner: Arc<MemInner>,
}

impl Default for MemDir {
    fn default() -> Self {
        Self::new()
    }
}

fn not_found(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{name}: no such file"))
}

fn parent_of(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(dir, _)| dir)
}

impl MemState {
    fn is_dir(&self, name: &str) -> bool {
        name.is_empty() || self.dirs.contains(name)
    }

    fn file(&self, name: &str) -> io::Result<&Arc<Vec<u8>>> {
        self.files.get(name).ok_or_else(|| not_found(name))
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

    /// The names below `dir` (`dir/...`), files and directories.
    fn below(&self, dir: &str) -> (Vec<String>, Vec<String>) {
        let prefix = format!("{dir}/");
        let files = self
            .files
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        let dirs = self
            .dirs
            .iter()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        (files, dirs)
    }
}

impl MemDir {
    /// An empty directory.
    pub fn new() -> Self {
        Self::named("memory")
    }

    /// An empty directory whose [`StoreDir::location`] is `name` (diagnostics, a list of
    /// databases).
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(MemInner {
                name: name.into(),
                ..MemInner::default()
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, MemState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The bytes held by all files.
    pub fn total_bytes(&self) -> u64 {
        self.state().files.values().map(|f| f.len() as u64).sum()
    }

    /// Every file name, sorted.
    pub fn file_names(&self) -> Vec<String> {
        self.state().files.keys().cloned().collect()
    }

    /// The bytes of file `name` (direct access past the Store: tests and tools).
    pub fn file(&self, name: &str) -> Option<Vec<u8>> {
        self.state().files.get(name).map(|f| f.as_ref().clone())
    }

    /// Replaces the bytes of file `name`, creating it (and its directories) when missing:
    /// direct access past the Store (tests: damage and crash simulations).
    pub fn put_file(&self, name: &str, bytes: Vec<u8>) {
        let mut state = self.state();
        let mut dir = parent_of(name);
        while !dir.is_empty() {
            state.dirs.insert(dir.to_string());
            dir = parent_of(dir);
        }
        state.files.insert(name.to_string(), Arc::new(bytes));
    }

    /// An independent copy of the directory's files and directories (not its locks): the
    /// state a crash at this moment leaves behind, or a copy to change without touching this
    /// one.
    pub fn deep_copy(&self) -> MemDir {
        let state = self.state();
        let copy = MemDir::named(self.inner.name.clone());
        {
            let mut target = copy.state();
            target.files = state
                .files
                .iter()
                .map(|(k, v)| (k.clone(), Arc::new(v.as_ref().clone())))
                .collect();
            target.dirs = state.dirs.clone();
        }
        copy
    }
}

/// A reader of a [`MemDir`] file: the bytes as they were when it was opened.
struct MemReader {
    bytes: Arc<Vec<u8>>,
    at: usize,
}

impl Read for MemReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let rest = self.bytes.get(self.at..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        self.at += n;
        Ok(n)
    }
}

/// A file of a [`MemDir`] open for writing.
struct MemFile {
    dir: MemDir,
    name: String,
    at: u64,
}

impl Write for MemFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.dir.state();
        let file = state
            .files
            .get_mut(&self.name)
            .ok_or_else(|| not_found(&self.name))?;
        let bytes = Arc::make_mut(file);
        let at = usize::try_from(self.at).map_err(|_| io::Error::other("file too large"))?;
        if bytes.len() < at {
            bytes.resize(at, 0);
        }
        let overlap = (bytes.len() - at).min(buf.len());
        bytes[at..at + overlap].copy_from_slice(&buf[..overlap]);
        bytes.extend_from_slice(&buf[overlap..]);
        self.at += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StoreFile for MemFile {
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.dir.truncate(&self.name, len)?;
        self.at = len;
        Ok(())
    }
}

/// An in-process lock of a [`MemDir`]: released when dropped.
struct MemLock {
    dir: MemDir,
    kind: LockKind,
}

impl Drop for MemLock {
    fn drop(&mut self) {
        let mut state = self.dir.state();
        match self.kind {
            LockKind::Writer => state.writer = false,
            LockKind::BackupShared => state.backups = state.backups.saturating_sub(1),
            LockKind::BackupExclusive => state.pruning = false,
        }
        drop(state);
        self.dir.inner.released.notify_all();
    }
}

impl StoreDir for MemDir {
    fn location(&self) -> String {
        self.inner.name.clone()
    }

    fn is_volatile(&self) -> bool {
        true
    }

    fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>> {
        let state = self.state();
        Ok(if state.files.contains_key(name) {
            Some(EntryKind::File)
        } else if state.is_dir(name) {
            Some(EntryKind::Dir)
        } else {
            None
        })
    }

    fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>> {
        let state = self.state();
        if !state.is_dir(dir) {
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
        let mut out: Vec<DirEntry> = state
            .files
            .keys()
            .filter_map(direct)
            .map(|name| DirEntry::new(name, EntryKind::File))
            .collect();
        out.extend(
            state
                .dirs
                .iter()
                .filter_map(direct)
                .map(|name| DirEntry::new(name, EntryKind::Dir)),
        );
        Ok(out)
    }

    fn file_len(&self, name: &str) -> io::Result<u64> {
        Ok(self.state().file(name)?.len() as u64)
    }

    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        let state = self.state();
        let file = state.file(name)?;
        if file.len() as u64 > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{name} is larger than {max} bytes"),
            ));
        }
        Ok(file.as_ref().clone())
    }

    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let state = self.state();
        let file = state.file(name)?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        start
            .checked_add(len)
            .and_then(|end| file.get(start..end))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{name} is shorter than {offset} + {len} bytes"),
                )
            })
    }

    fn open_read(&self, name: &str) -> io::Result<Box<dyn Read + Send>> {
        let bytes = self.state().file(name)?.clone();
        Ok(Box::new(MemReader { bytes, at: 0 }))
    }

    fn create_dir_all(&self, name: &str) -> io::Result<()> {
        let mut state = self.state();
        let mut dir = name;
        while !dir.is_empty() {
            if state.files.contains_key(dir) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{dir} is a file"),
                ));
            }
            state.dirs.insert(dir.to_string());
            dir = parent_of(dir);
        }
        Ok(())
    }

    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        {
            let mut state = self.state();
            state.check_parent(name)?;
            if state.dirs.contains(name) {
                return Err(io::Error::other(format!("{name} is a directory")));
            }
            if exclusive && state.files.contains_key(name) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{name} exists"),
                ));
            }
            state.files.insert(name.to_string(), Arc::new(Vec::new()));
        }
        Ok(Box::new(MemFile {
            dir: self.clone(),
            name: name.to_string(),
            at: 0,
        }))
    }

    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        self.state().file(name)?;
        Ok(Box::new(MemFile {
            dir: self.clone(),
            name: name.to_string(),
            at: offset,
        }))
    }

    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.state();
        state.check_parent(name)?;
        let file = state.files.entry(name.to_string()).or_default();
        Arc::make_mut(file).extend_from_slice(bytes);
        Ok(())
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.state();
        state.check_parent(name)?;
        state
            .files
            .insert(name.to_string(), Arc::new(bytes.to_vec()));
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let mut state = self.state();
        state.check_parent(to)?;
        if let Some(file) = state.files.remove(from) {
            if state.dirs.contains(to) {
                state.files.insert(from.to_string(), file);
                return Err(io::Error::other(format!("{to} is a directory")));
            }
            state.files.insert(to.to_string(), file);
            return Ok(());
        }
        if !state.dirs.contains(from) {
            return Err(not_found(from));
        }
        if state.dirs.contains(to) {
            // The contract's completed move (see the trait): only the source goes.
            let (files, dirs) = state.below(from);
            for name in files {
                state.files.remove(&name);
            }
            for name in dirs {
                state.dirs.remove(&name);
            }
            state.dirs.remove(from);
            return Ok(());
        }
        if state.files.contains_key(to) {
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
        let (files, dirs) = state.below(from);
        let moved = |name: &str| format!("{to}{}", &name[from.len()..]);
        for name in files {
            if let Some(file) = state.files.remove(&name) {
                state.files.insert(moved(&name), file);
            }
        }
        for name in dirs {
            state.dirs.remove(&name);
            state.dirs.insert(moved(&name));
        }
        state.dirs.remove(from);
        state.dirs.insert(to.to_string());
        Ok(())
    }

    fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
        let mut state = self.state();
        let file = state.files.get_mut(name).ok_or_else(|| not_found(name))?;
        let len = usize::try_from(len).map_err(|_| io::Error::other("file too large"))?;
        Arc::make_mut(file).resize(len, 0);
        Ok(())
    }

    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.state()
            .files
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| not_found(name))
    }

    fn remove_tree(&self, name: &str) -> io::Result<()> {
        let mut state = self.state();
        if state.files.remove(name).is_some() || name.is_empty() {
            return Ok(());
        }
        if state.dirs.remove(name) {
            let (files, dirs) = state.below(name);
            for file in files {
                state.files.remove(&file);
            }
            for dir in dirs {
                state.dirs.remove(&dir);
            }
        }
        Ok(())
    }

    fn sync_dir(&self, _: &str) -> io::Result<()> {
        Ok(())
    }

    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
        let mut state = self.state();
        match kind {
            LockKind::Writer => {
                if state.writer {
                    return Ok(None);
                }
                state.writer = true;
            }
            LockKind::BackupExclusive => {
                if state.pruning || state.backups > 0 {
                    return Ok(None);
                }
                state.pruning = true;
            }
            LockKind::BackupShared => {
                // Only another thread can hold the prune pin (a prune runs to its end without
                // yielding), so single-threaded targets never wait here.
                while state.pruning {
                    state = self
                        .inner
                        .released
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                state.backups += 1;
            }
        }
        drop(state);
        Ok(Some(DirLockGuard::new(MemLock {
            dir: self.clone(),
            kind,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_dir_names_files_and_directories() {
        let dir = MemDir::new();
        assert!(dir.create("a/b", false).is_err(), "no parent directory");
        dir.create_dir_all("a/x").unwrap();
        let mut file = dir.create("a/b", true).unwrap();
        file.write_all(b"hello").unwrap();
        assert!(dir.create("a/b", true).is_err());
        assert_eq!(dir.read("a/b", 10).unwrap(), b"hello");
        assert!(dir.read("a/b", 4).is_err());
        assert_eq!(dir.read_range("a/b", 1, 3).unwrap(), b"ell");
        assert!(dir.read_range("a/b", 3, 3).is_err());
        file.set_len(2).unwrap();
        file.write_all(b"y!").unwrap();
        assert_eq!(dir.file("a/b").unwrap(), b"hey!");
        let mut names: Vec<_> = dir.list("a").unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, ["b", "x"]);
        dir.append("a/x/m", b"1").unwrap();
        dir.append("a/x/m", b"2").unwrap();
        dir.rename("a", "c").unwrap();
        assert_eq!(dir.entry_kind("a").unwrap(), None);
        assert_eq!(dir.file("c/x/m").unwrap(), b"12");
        assert_eq!(dir.entry_kind("c/x").unwrap(), Some(EntryKind::Dir));
        let copy = dir.deep_copy();
        dir.remove_tree("c").unwrap();
        assert!(dir.file_names().is_empty());
        assert_eq!(copy.file_names(), ["c/b", "c/x/m"]);
        assert_eq!(copy.total_bytes(), 6);
    }

    #[test]
    fn mem_dir_locks_are_in_process_flags() {
        let dir = MemDir::new();
        let writer = dir.lock(LockKind::Writer).unwrap();
        assert!(writer.is_some());
        assert!(dir.lock(LockKind::Writer).unwrap().is_none());
        drop(writer);
        assert!(dir.lock(LockKind::Writer).unwrap().is_some());
        let backup = dir.lock(LockKind::BackupShared).unwrap();
        assert!(dir.lock(LockKind::BackupExclusive).unwrap().is_none());
        drop(backup);
        assert!(dir.lock(LockKind::BackupExclusive).unwrap().is_some());
    }
}
