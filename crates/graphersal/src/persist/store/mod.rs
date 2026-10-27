//! The Store: one directory = one graph (format spec sections 4-6, 9-13) over a
//! [`StoreDir`](super::StoreDir): a path on disk (native) or a `MemDir` (every target).
//!
//! A [`Store`] owns a graph in memory and keeps it on disk: a snapshot directory as the base and
//! a write-ahead log of WAL segment files with every commit since. Opening a store runs the
//! recovery (finish an interrupted operation, remove temporary files, load the latest snapshot,
//! replay the WAL, cut an interrupted last write) and attaches the journal, so every commit of
//! the graph is durable before it returns. See the book's "The Store" pages
//! (`book/src/persistence/store.md`).
//!
//! ```rust
//! use graphersal::persist::{Store, StoreOptions};
//!
//! let dir = std::env::temp_dir().join(format!("doc-store-{}", std::process::id()));
//! # let _ = std::fs::remove_dir_all(&dir);
//! let store = Store::create(&dir, StoreOptions::new()).unwrap();
//! store.graph().write().traversal_mut().add_v("person").property("name", "ann").to_list().unwrap();
//! store.mark("after-ann").unwrap();
//! store.close().unwrap();
//!
//! let store = Store::open(&dir, StoreOptions::new()).unwrap();
//! assert_eq!(store.graph().read().vertex_count(), 1);
//! assert_eq!(store.marks().unwrap()[0].name(), "after-ann");
//! # drop(store);
//! # std::fs::remove_dir_all(&dir).unwrap();
//! ```

mod backup;
mod backup_check;
mod compaction;
mod damage;
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
mod embedders;
mod files;
mod from_memory;
mod graph;
mod licenses;
mod maintenance;
mod merge;
mod models;
mod rollback;
mod snapshot_dir;
mod upgrade;
mod wal_files;
mod workload;
#[cfg(feature = "persist-zip")]
mod zip;

use crate::graph::PersistentStorage;
use crate::persist::StoreFailure;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::codec::Codec;
use super::dir::{IntoStoreDir, SpaceUsage, StoreDir};
use super::error::PersistError;
use super::journal::{Durability, DurableEnd, DurablePosition, Journal};
use super::recover::{Recovered, RecoveryTarget};
use super::segment::DEFAULT_CHUNK_BYTES;
use super::snapshot::{ReadOptions, SnapshotInfo, WriteOptions};
use crate::graph::hooks::Mark;
use crate::graph::{Graph, TraversalGraph};
use graph::{GraphMaker, JournalCell, LiveWrite, LoadPlan, StoreGraph, TraversalGraphs};

pub use backup::BackupMode;
pub use compaction::{AdviceRule, CompactReport, CompactionAdvice, CompactionPolicy};
pub use damage::{Damage, DamageKind, DamageReport, Diverged, Donor, IdRange, RepairReport};
#[cfg(all(feature = "model2vec", not(target_arch = "wasm32")))]
pub use embedders::StoredEmbedders;
pub use files::{BackupMarker, DamagePolicy, LineageEntry};
pub use maintenance::{BackupReport, ChangesSince, PruneReport, VerifyProblem, VerifyReport};
pub use models::{
    AddModelReport, BaseModel, ModelCopy, ModelFile, ModelRole, NewModel, StoredModel,
};
pub use rollback::{AtticEntry, ForkReport, RollbackMode, RollbackReport};
pub use upgrade::UpgradeReport;
pub(crate) use upgrade::upgradable_from;
pub use workload::{NotScheduledJob, UserJobs};

pub(crate) use files::STORE_VERSION;
use files::{
    DirLock, DirPath, GraphFile, Intent, SNAPSHOTS_DIR, TMP_PREFIX, WAL_DIR, list_snapshots,
    list_wal, read_graph_file, remove_path, safe_relative, wal_after, write_graph_file,
};
use snapshot_dir::{
    DirSnapshotReader, NewSnapshotDir, read_listing, read_manifest, write_graph_snapshot,
};
use wal_files::{
    Faults, StoreWalFiles, WalShared, WalWriter, append_mark, create_segment, open_segment_at,
    open_segment_reader, reachable_marks, read_marks, record_wal_head, scan_marks,
    wal_set_problems, write_marks,
};

/// The default size at which a WAL segment closes (16 MiB, as PostgreSQL's).
pub const DEFAULT_WAL_SEGMENT_BYTES: u64 = 16 << 20;
/// The default WAL volume since the last snapshot that triggers an automatic checkpoint:
/// 256 MiB natively, 4 MiB in the browser (wasm32, where the store lives in memory and the
/// checkpoint runs synchronously, see [`Store::run_due_checkpoint`]).
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_CHECKPOINT_AFTER_BYTES: u64 = 256 << 20;
/// The default WAL volume since the last snapshot that triggers an automatic checkpoint:
/// 256 MiB natively, 4 MiB in the browser (wasm32, where the store lives in memory and the
/// checkpoint runs synchronously, see [`Store::run_due_checkpoint`]).
#[cfg(target_arch = "wasm32")]
pub const DEFAULT_CHECKPOINT_AFTER_BYTES: u64 = 4 << 20;

/// The default snapshot segment target of a store (64 MiB): a merge checkpoint reuses every
/// segment the WAL since the last snapshot does not touch and rewrites the touched ones whole,
/// so a store keeps its segments smaller than a packed snapshot's
/// ([`DEFAULT_SEGMENT_BYTES`](super::DEFAULT_SEGMENT_BYTES)).
pub const DEFAULT_STORE_SEGMENT_BYTES: u64 = 64 << 20;

/// The default bound of a merge checkpoint: the WAL bytes after the last snapshot it merges
/// (1 GiB natively, 64 MiB in the browser). Beyond it a checkpoint encodes the loaded graph
/// instead ([`StoreOptions::with_checkpoint_merge_bytes`]).
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_CHECKPOINT_MERGE_BYTES: u64 = 1 << 30;
/// The default bound of a merge checkpoint: the WAL bytes after the last snapshot it merges
/// (1 GiB natively, 64 MiB in the browser). Beyond it a checkpoint encodes the loaded graph
/// instead ([`StoreOptions::with_checkpoint_merge_bytes`]).
#[cfg(target_arch = "wasm32")]
pub const DEFAULT_CHECKPOINT_MERGE_BYTES: u64 = 64 << 20;

/// Options of a [`Store`]. The chunk size is fixed when the store is created (it is a
/// directory setting in `GRAPH`); everything else applies per open.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoreOptions {
    durability: Durability,
    wal_segment_bytes: u64,
    checkpoint_after_bytes: u64,
    checkpoint_merge_bytes: u64,
    chunk_bytes: u32,
    snapshot_segment_bytes: u64,
    codec: Codec,
    checkpoint_on_close: bool,
    read: ReadOptions,
    compaction: CompactionPolicy,
    auto_compact: bool,
    damage_policy: DamagePolicy,
    workload_level: crate::graph::workload::WorkloadLevel,
    /// Injected WAL failures (crate tests only).
    faults: Option<Arc<Faults>>,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            durability: Durability::EveryCommit,
            wal_segment_bytes: DEFAULT_WAL_SEGMENT_BYTES,
            checkpoint_after_bytes: DEFAULT_CHECKPOINT_AFTER_BYTES,
            checkpoint_merge_bytes: DEFAULT_CHECKPOINT_MERGE_BYTES,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            snapshot_segment_bytes: DEFAULT_STORE_SEGMENT_BYTES,
            codec: Codec::Lz4,
            checkpoint_on_close: false,
            read: ReadOptions::default(),
            compaction: CompactionPolicy::default(),
            auto_compact: false,
            damage_policy: DamagePolicy::Maintenance,
            workload_level: crate::graph::workload::WorkloadLevel::Persisted,
            faults: None,
        }
    }
}

impl StoreOptions {
    /// The defaults: fsync per commit, WAL segments of 16 MiB, an automatic checkpoint after
    /// 256 MiB of WAL, checkpoints merged from the last snapshot and up to 1 GiB of WAL, LZ4
    /// chunks of 1 MiB, snapshot segments of 64 MiB, no checkpoint on close.
    pub fn new() -> Self {
        Self::default()
    }

    /// How much workload statistics the store keeps (book page "Workload Statistics"; default
    /// [`WorkloadLevel::Persisted`](crate::workload::WorkloadLevel::Persisted): in memory and in
    /// the sidecar file `WORKLOAD`, read back at the next open). Per open, never stored in
    /// `GRAPH`; `off` keeps nothing, `memory` writes no file.
    pub fn with_workload_level(mut self, level: crate::graph::workload::WorkloadLevel) -> Self {
        self.workload_level = level;
        self
    }

    /// When a commit is durable ([`Durability::EveryCommit`] by default).
    pub fn with_durability(mut self, durability: Durability) -> Self {
        self.durability = durability;
        self
    }

    /// The size after which a WAL segment closes (a record is never split).
    pub fn with_wal_segment_bytes(mut self, bytes: u64) -> Self {
        self.wal_segment_bytes = bytes.max(1);
        self
    }

    /// The WAL volume since the last snapshot that starts an automatic checkpoint: in a
    /// background thread where the platform has threads, otherwise at the next
    /// [`Store::run_due_checkpoint`] (`0`: never; checkpoint by hand).
    pub fn with_checkpoint_after_bytes(mut self, bytes: u64) -> Self {
        self.checkpoint_after_bytes = bytes;
        self
    }

    /// The bound of a merge checkpoint (format spec 12.3; default
    /// [`DEFAULT_CHECKPOINT_MERGE_BYTES`]): a checkpoint builds the new snapshot from the last
    /// one and the WAL after it, without the graph's lock and with memory proportional to that
    /// WAL, as long as the WAL after the last snapshot holds at most `bytes`; beyond it (and
    /// with `0`, never merge) it encodes the loaded graph under its read lock, as
    /// [`Store::checkpoint_dir`] does after loading the store.
    pub fn with_checkpoint_merge_bytes(mut self, bytes: u64) -> Self {
        self.checkpoint_merge_bytes = bytes;
        self
    }

    /// The uncompressed chunk size of snapshots (default 1 MiB): used by [`Store::create`]
    /// only and fixed for the directory's life (the unit of repair).
    pub fn with_chunk_bytes(mut self, bytes: u32) -> Self {
        self.chunk_bytes = bytes.max(1);
        self
    }

    /// The snapshot segment target size (default [`DEFAULT_STORE_SEGMENT_BYTES`], 64 MiB): the
    /// unit a merge checkpoint reuses or rewrites.
    pub fn with_snapshot_segment_bytes(mut self, bytes: u64) -> Self {
        self.snapshot_segment_bytes = bytes.max(1);
        self
    }

    /// The compression of snapshot chunks and large WAL records ([`Codec::Lz4`] by default).
    pub fn with_codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// Whether [`Store::close`] writes a checkpoint first (shorter replay at the next open).
    pub fn with_checkpoint_on_close(mut self, yes: bool) -> Self {
        self.checkpoint_on_close = yes;
        self
    }

    /// The read options of loads (memory limit, value depth).
    pub fn with_read_options(mut self, read: ReadOptions) -> Self {
        self.read = read;
        self
    }

    /// The rule of [`Store::compaction_advice`] (and of the automatic compaction).
    pub fn with_compaction_policy(mut self, policy: CompactionPolicy) -> Self {
        self.compaction = policy;
        self
    }

    /// Whether [`Store::prune`] compacts the backend afterwards when
    /// [`Store::compaction_advice`] recommends it (default `false`: compaction is explicit,
    /// [`Store::compact`]). Only a single-file store keeps garbage to compact.
    pub fn with_auto_compact(mut self, yes: bool) -> Self {
        self.auto_compact = yes;
        self
    }

    /// What the store does when damage is found on disk while it is open (default
    /// [`DamagePolicy::Maintenance`]: it turns read-only; [`DamagePolicy::Continue`]: it keeps
    /// committing and reports the damage). A CREATION parameter like the chunk size: used by
    /// [`Store::create`] (and the other ways to create a store) only, stored in the store's
    /// `GRAPH` file (format spec 5.2 bit 5) and never changed afterwards; an open ignores it
    /// ([`Store::damage_policy`] says what the store has). Forks, repairs, conversions,
    /// backups and restores carry the store's own policy.
    pub fn with_damage_policy(mut self, policy: DamagePolicy) -> Self {
        self.damage_policy = policy;
        self
    }
}

/// The WAL tail cut when a store was opened: an interrupted last write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CutTail {
    /// The WAL segment (its first commit sequence number, the file name).
    pub segment: u64,
    /// Where the incomplete record started (the new end of the file).
    pub offset: u64,
    /// How many bytes were cut.
    pub bytes: u64,
}

/// What opening a store did (the recovery report).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct OpenReport {
    /// Whether the store was closed properly last time (`false`: a crash, or a writer that
    /// never closed it).
    pub clean_close: bool,
    /// The interrupted multi-file operation that was completed (`"prune"`, ...).
    pub intent_completed: Option<String>,
    /// Temporary files and directories removed (an interrupted checkpoint or rewrite).
    pub tmp_removed: usize,
    /// One of `GRAPH` / `GRAPH.copy` was missing, damaged or stale and was rewritten.
    pub graph_file_repaired: Option<String>,
    /// The interrupted last WAL write that was cut off.
    pub torn_tail: Option<CutTail>,
    /// The position of the snapshot loaded.
    pub snapshot_commit_seq: u64,
    /// The position after the replay.
    pub commit_seq: u64,
    /// WAL commits replayed onto the snapshot.
    pub commits_replayed: u64,
    /// The `marks` file was missing or damaged and was rebuilt from the WAL.
    pub marks_rebuilt: bool,
    /// A refused WAL record that could not be cut off when it was refused (a failed `fsync`
    /// followed by a failed truncation) and was cut now: it is never replayed.
    pub refused_cut: Option<CutTail>,
    /// Damage of a redundant copy that did not stop the open (one copy of the latest
    /// snapshot manifest's fixed part); `verify` lists it, `repair_to` writes fresh copies.
    pub warnings: Vec<String>,
    /// The store opened in maintenance mode: damage was found, it is read-only
    /// ([`Store::maintenance`] has the report).
    pub maintenance: bool,
    /// The store opened read-only because it holds something critical this version does not
    /// know (format spec 16 rule 3): the reason ([`Store::read_only_reason`]).
    pub read_only: Option<String>,
}

/// A store's identity, position and files ([`Store::info`], [`Store::inspect`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct StoreInfo {
    /// Where the store is ([`StoreDir::location`]: a path, `memory:main`).
    pub location: String,
    /// The format version of the directory.
    pub format_version: u32,
    /// The current lineage id.
    pub graph_id: u128,
    /// The store's identity: a creation parameter (format spec 5.1), random, made by a
    /// create. A fork, attic fork, repair or conversion is another store with a new one; an
    /// in-place rollback, an attic restore and a restored backup keep it. A backup records its
    /// source's, and an incremental backup requires the same (format spec 12.7).
    pub store_id: u128,
    /// The lineage chain, newest first (a fork lists its parents).
    pub lineage: Vec<LineageEntry>,
    /// When the store was created.
    pub created_at: i64,
    /// The snapshot chunk size of the directory.
    pub chunk_bytes: u32,
    /// The last commit (of the open graph, or the last durable one in the files).
    pub commit_seq: u64,
    /// Its time (`0`: none yet).
    pub last_commit_at: i64,
    /// The durable end of the WAL (an open store; `None` from [`Store::inspect`]).
    pub durable: Option<DurablePosition>,
    /// The snapshots, oldest first (listing fields only: `meta` is empty).
    pub snapshots: Vec<SnapshotInfo>,
    /// The marks a target can reach, in order (see [`Store::marks`]).
    pub marks: Vec<Mark>,
    /// WAL segment files.
    pub wal_segments: usize,
    /// Their total size in bytes.
    pub wal_bytes: u64,
    /// Whether the last close was clean (`false` while a writer has it open).
    pub clean: bool,
    /// The attic: history moved aside by in-place rollbacks.
    pub attic: Vec<AtticEntry>,
    /// The bytes of the store's snapshots (their files).
    pub snapshot_bytes: u64,
    /// The backup marker: the directory is a backup, read-only until it is restored
    /// ([`Store::restore_backup`]).
    pub backup: Option<BackupMarker>,
    /// The physical space of a backend that keeps garbage (a single-file store: its size and
    /// live bytes); `None` for a directory.
    pub space: Option<SpaceUsage>,
    /// The definitions of the catalog (saved queries and every other kind): of the open graph
    /// ([`Store::info`]), or from the files ([`Store::inspect`]: the latest snapshot's
    /// definitions table with the WAL's catalog changes after it applied).
    pub definitions: u64,
    /// [`Self::definitions`] per kind, by the kind's name (`query`, `compression`, or
    /// `kind <n>` for a kind this version does not know).
    pub definitions_by_kind: std::collections::BTreeMap<String, u64>,
    /// What the store does when damage is found on disk while it is open: a creation
    /// parameter, fixed for the store's life (format spec 5.2 bit 5).
    pub damage_policy: DamagePolicy,
    /// The damage found on disk while the store is open ([`Store::damage_found`]); empty from
    /// [`Store::inspect`].
    pub damage_found: Vec<FoundDamage>,
    /// Whether the store turned read-only because damage was found while it is open (its
    /// damage policy `maintenance`; [`PersistError::DamageFound`]).
    pub frozen_by_damage: bool,
    /// Why the store stopped after an in-place rollback or attic restore failed part-way
    /// ([`Store::stopped_reason`]); `None`: not stopped (always from [`Store::inspect`]).
    pub stopped: Option<String>,
    /// The embedding models the store keeps (format spec 4.5), with their licences
    /// ([`Store::models`]).
    pub models: Vec<StoredModel>,
    /// The model identities the schema's `embedder` keywords name (of the open graph, or from
    /// the files for [`Store::inspect`]): a stored model whose identity is among them is in
    /// use; one named here but not stored nor registered is missing.
    pub schema_models: Vec<String>,
}

/// Damage found ON DISK while the store was open (by a backup, a checkpoint, a verification):
/// [`Store::damage_found`]. What the store does about it is its [`DamagePolicy`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FoundDamage {
    /// What found it: `"backup"`, `"checkpoint"` or `"verify"`.
    pub found_by: String,
    /// When, microseconds since the Unix epoch, UTC.
    pub at: i64,
    /// The problems, as [`Store::verify`] reports them (paths relative to the store).
    pub problems: Vec<VerifyProblem>,
}

/// What a [`Store`] shares with its checkpoint thread.
struct Shared {
    dir: DirPath,
    /// The live graph, seen through the object-safe [`StoreGraph`].
    graph: Arc<dyn StoreGraph>,
    /// The journal in the graph's journal slot (empty in a read-only, maintenance or backup
    /// store, and after a reload that failed or went read-only).
    journal: JournalCell,
    options: StoreOptions,
    chunk_bytes: u32,
    graph_file: Arc<Mutex<GraphFile>>,
    wal: Arc<WalShared>,
    durable: DurableEnd,
    /// Serializes checkpoint, prune and close.
    maintenance: Mutex<()>,
    shutdown: AtomicBool,
    last_auto_error: Mutex<Option<Arc<PersistError>>>,
    /// Why the store is read-only (format spec 16 rule 3: something critical this version does
    /// not know); `None`: writable (unless in maintenance mode, a backup, or closed).
    read_only: Mutex<Option<String>>,
    /// Why the store stopped after an in-place rollback or attic restore failed part-way
    /// ([`Store::stopped_reason`]); `None`: not stopped.
    stopped: Mutex<Option<String>>,
    /// Damage found on disk while the store is open, in the order it was found.
    found: Mutex<Vec<FoundDamage>>,
    /// The workload statistics (`workload.rs`): read from the sidecar at open, written by the
    /// system job `stats_flush` and at close.
    workload: Arc<crate::graph::workload::WorkloadStats>,
}

/// One graph kept in one directory: snapshot + write-ahead log, recovery on open, checkpoints,
/// marks, point-in-time reads, fork, prune, verify and backup (the storage format spec).
///
/// The directory is a [`StoreDir`]: a path of the file system ([`FsDir`](super::FsDir), what
/// every `impl IntoStoreDir` path argument becomes; native only) or a [`MemDir`](super::MemDir)
/// (in memory, also in the browser: the same functionality, gone with the process).
///
/// - **One writer**: the directory's `LOCK` is held (OS lock) while the store is open; a second
///   open, in this or another process, fails with [`PersistError::Locked`].
/// - **Durable commits**: every commit of [`Store::graph`] is written to the WAL (and synced
///   per [`Durability`]) before it returns; a failed write rolls the commit back. After
///   [`Store::close`] the graph refuses commits.
/// - **Checkpoints** write a new snapshot MERGED from the last one and the WAL after it, without
///   the graph's lock (commits go on), by hand ([`Store::checkpoint`]), in the background when
///   the WAL since the last snapshot exceeds [`StoreOptions::with_checkpoint_after_bytes`]
///   (without threads, wasm32: at the next [`Store::run_due_checkpoint`]), or on a closed store
///   ([`Store::checkpoint_dir`]). Nothing is ever deleted implicitly: [`Store::prune`]
///   removes old snapshots and WAL explicitly.
///
/// # Over your own storage
///
/// `Store` is generic over the graph's storage: `Store` alone is `Store<TraversalGraph>`
/// (opened by [`Store::open`], created by [`Store::create`]); a host's own
/// [`PersistentStorage`] (EXPERIMENTAL) is kept by a `Store<S>` opened with
/// [`Store::open_in`] or created with [`Store::create_in`]. The files are the same for every
/// storage type (a store written through one opens through any other), and every method below
/// works for every `S`. The storage must claim the capabilities `atomic` and `change_capture`
/// ([`StorageCapabilities`](crate::graph::StorageCapabilities)), else the open fails with
/// [`StoreFailure::MissingCapability`]. Internally a merge checkpoint, a salvage and a repair
/// still work on a [`TraversalGraph`] of their own (they write files, never the host's graph);
/// a store opened in maintenance mode copies the salvaged state into a new `S`.
pub struct Store<S: PersistentStorage = TraversalGraph> {
    /// Everything but the typed graph: non-generic, works on the graph as `dyn StoreGraph`.
    core: Arc<StoreCore>,
    /// The same graph as the core's, typed.
    graph: Arc<Graph<S>>,
}

/// The Store's non-generic core (`book/src/for_developers/custom_storage.md`): every operation
/// of [`Store`] runs here, on the live graph as [`StoreGraph`]; `Store` adds the typed graph.
struct StoreCore {
    shared: Arc<Shared>,
    report: OpenReport,
    /// Set when the store opened in maintenance mode (damage found): read-only.
    maintenance: Option<DamageReport>,
    /// Set when the store is a backup opened read-only ([`Store::open_backup`]).
    backup: Option<BackupMarker>,
    lock: Mutex<Option<DirLock>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    closed: AtomicBool,
}

impl<S: PersistentStorage> std::fmt::Debug for Store<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("dir", &self.core.shared.dir.display())
            .field("report", &self.core.report)
            .finish_non_exhaustive()
    }
}

fn now() -> i64 {
    crate::graph::undo::now_micros()
}

/// Prepares a directory for a new store: it must not exist or be empty.
fn prepare_new_dir(dir: &DirPath) -> Result<(), PersistError> {
    if files::has_entries(dir) {
        return Err(PersistError::DirectoryNotEmpty {
            path: dir.display(),
        });
    }
    for sub in [SNAPSHOTS_DIR, WAL_DIR] {
        dir.join(sub)
            .create_dir_all()
            .map_err(|err| PersistError::io("creating the store directory", err))?;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(parent) = dir.store_dir().local_path().and_then(|p| p.parent()) {
        let _ = super::FsDir::new(parent).sync_dir("");
    }
    files::sync_dir(dir)
}

/// Removes what a failed create left (the directory's contents; the directory itself only when
/// it did not exist before).
fn undo_create(dir: &DirPath, existed: bool) {
    files::undo_new_dir(dir, existed);
}

impl Store {
    /// Opens the store in `dir` (see [`Store::open`]).
    fn open_at(dir: DirPath, options: StoreOptions) -> Result<Store, PersistError> {
        Self::from_core(StoreCore::open_at(dir, options, &TraversalGraphs)?)
    }

    /// Creates a store for an empty graph in `dir` (which must not exist or be empty) and opens
    /// it.
    pub fn create(dir: impl IntoStoreDir, options: StoreOptions) -> Result<Store, PersistError> {
        Self::create_with(dir, TraversalGraph::new(), options)
    }

    /// Creates a store whose first snapshot is `graph` (its data, schema, lineage id, commit
    /// position and auto-id sequences) and opens it.
    pub fn create_with(
        dir: impl IntoStoreDir,
        graph: TraversalGraph,
        options: StoreOptions,
    ) -> Result<Store, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        let lineage = vec![LineageEntry {
            graph_id: graph.graph_id(),
            branched_at: 0,
            time: now(),
        }];
        StoreCore::create_lineage(&dir, &graph, lineage, WriteOptions::new(), &options)?;
        Self::open_at(dir, options)
    }

    /// Creates a store from a packed snapshot (`.gsnap`): its sections become the first
    /// snapshot directory by copying (format spec 8), then the store is opened.
    pub fn create_from_packed(
        dir: impl IntoStoreDir,
        packed: impl std::io::Read,
        options: StoreOptions,
    ) -> Result<Store, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        StoreCore::create_from_packed_at(&dir, packed, &options)?;
        Self::open_at(dir, options)
    }

    /// Opens the store in `dir` read-write and runs the recovery: takes the `LOCK`, completes
    /// an interrupted multi-file operation (`INTENT`), removes temporary files, loads the
    /// latest snapshot, replays the WAL after it, cuts an interrupted last write (a torn tail;
    /// damage anywhere fails the open), rebuilds a missing `marks` file, attaches the journal
    /// and marks the store open. [`Store::open_report`] says what it did.
    pub fn open(dir: impl IntoStoreDir, options: StoreOptions) -> Result<Store, PersistError> {
        Self::open_at(DirPath::root(dir.into_store_dir()), options)
    }

    /// Creates a SINGLE-FILE store at `path` for an empty graph (a
    /// [`SingleFileDir`](super::SingleFileDir), whatever the extension; native only) and
    /// opens it. A path with the extension `.gstore` becomes one through [`Store::create`] too.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn create_file(
        path: impl AsRef<std::path::Path>,
        options: StoreOptions,
    ) -> Result<Store, PersistError> {
        Self::create(super::SingleFileDir::new(path), options)
    }

    /// Opens the single-file store at `path` read-write ([`Store::open`] of a
    /// [`SingleFileDir`](super::SingleFileDir); an existing file opens as one through
    /// [`Store::open`] too).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_file(
        path: impl AsRef<std::path::Path>,
        options: StoreOptions,
    ) -> Result<Store, PersistError> {
        Self::open(super::SingleFileDir::new(path), options)
    }

    /// Copies the whole store in `from` into `to`, name by name and byte for byte: every
    /// snapshot, the WAL, the marks, the attic, `GRAPH` (last). `to` is a new or empty
    /// directory, or a new single file (`graphersal store convert`: directory <-> single
    /// file, or into memory). `from` must not be open by a writer (its lock is held while it
    /// is copied); the copy is verified. Nothing of `from` changes. The copy's `GRAPH` gets a
    /// NEW store id ([`StoreInfo::store_id`]): it is another store and does not continue the
    /// backups of `from`. Returns the files and bytes copied.
    pub fn convert(
        from: impl IntoStoreDir,
        to: impl IntoStoreDir,
    ) -> Result<(usize, u64), PersistError> {
        let from = DirPath::root(from.into_store_dir());
        let to = DirPath::root(to.into_store_dir());
        files::refuse_other_version(&from)?;
        if !Self::is_store(from.store_dir())? {
            return Err(PersistError::NotAStore {
                path: from.display(),
                reason: "it has no GRAPH file".into(),
            });
        }
        let _lock = DirLock::exclusive(&from)?;
        if files::has_entries(&to) {
            return Err(PersistError::DirectoryNotEmpty { path: to.display() });
        }
        let existed = to.exists();
        let result = (|| {
            let mut copied = (0usize, 0u64);
            let mut graph_files = Vec::new();
            copy_tree(&from, &to, &from, &mut copied, &mut graph_files)?;
            // GRAPH last, with a NEW store identity (format spec 5.1): the copy is another
            // store, so it cannot continue the original's backups (12.7).
            if !graph_files.is_empty() {
                let mut file = read_graph_file(&from)?.file;
                file.store_id = crate::value::uuid::new_v7();
                let bytes = file.encode();
                for name in graph_files {
                    files::write_atomic(&to, &name, &bytes)?;
                    copied.0 += 1;
                    copied.1 += bytes.len() as u64;
                }
            }
            files::sync_dir(&to)?;
            let verified = maintenance::verify(&to, None)?;
            if !verified.is_ok() {
                return Err(PersistError::store(
                    "convert the store",
                    StoreFailure::CopyDoesNotVerify,
                    format!(
                        "the copy in {} does not verify: {:?}",
                        to.display(),
                        verified.problems
                    ),
                ));
            }
            Ok(copied)
        })();
        if result.is_err() {
            undo_create(&to, existed);
        }
        result
    }

    /// [`Store::repair_to`] of the store in `dir`, without its lock (nothing in `dir` is
    /// written).
    pub fn repair_dir(
        dir: impl IntoStoreDir,
        new_dir: impl IntoStoreDir,
    ) -> Result<RepairReport, PersistError> {
        StoreCore::repair_at(
            &DirPath::root(dir.into_store_dir()),
            &DirPath::root(new_dir.into_store_dir()),
        )
    }

    /// The damage report of the store in `dir` without opening it (no lock, nothing
    /// written): what maintenance mode would show.
    pub fn damage_report_dir(dir: impl IntoStoreDir) -> Result<DamageReport, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        let dir = upgrade::older_view(&dir).unwrap_or(dir);
        Ok(damage::salvage(&dir, &ReadOptions::new().with_max_value_depth(4096))?.report)
    }

    /// Loads the store in `dir` read-only at `target` (format spec 12.1): the
    /// nearest snapshot at or before the target plus the WAL up to it. Takes no lock and
    /// changes no file, so it works while a writer has the store open (an incomplete last
    /// record is ignored). The graph is unpublished and without a journal: a past state.
    /// Read-write from a past target is [`Store::fork`].
    pub fn open_read_only(
        dir: impl IntoStoreDir,
        target: RecoveryTarget,
    ) -> Result<Recovered, PersistError> {
        StoreCore::load_at(
            &DirPath::root(dir.into_store_dir()),
            target,
            &ReadOptions::default(),
        )
    }

    /// Writes a snapshot of the CLOSED store in `dir` (`graphersal store checkpoint`): takes
    /// the writer lock (a store open elsewhere is refused with [`PersistError::Locked`]),
    /// completes an interrupted operation (`INTENT`) and removes temporary files as an open
    /// would, then merges the newest snapshot with the whole WAL after it (format spec 12.3)
    /// without loading the graph: memory proportional to that WAL, untouched segments reused
    /// by reference. A torn last record is ignored (the next open cuts it), a recorded cut of a
    /// refused record honoured; no Checkpoint record is appended and `GRAPH` is not changed.
    /// Beyond [`StoreOptions::with_checkpoint_merge_bytes`] the store is loaded and its graph
    /// encoded. `options` gives the codec, segment size and merge bound (the chunk size is the
    /// store's). Damage stops it with the error and nothing written.
    pub fn checkpoint_dir(
        dir: impl IntoStoreDir,
        options: StoreOptions,
        name: Option<&str>,
    ) -> Result<CheckpointReport, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_other_version(&dir)?;
        if !Self::is_store(dir.store_dir())? {
            return Err(PersistError::NotAStore {
                path: dir.display(),
                reason: "it has no GRAPH file".into(),
            });
        }
        let _lock = DirLock::exclusive(&dir)?;
        let backend = crate::persist::dir::backend_check(&**dir.store_dir());
        if !backend.damage.is_empty() {
            return Err(PersistError::corrupt(
                dir.display(),
                0,
                format!("the container is damaged: {}", backend.damage.join("; ")),
            ));
        }
        let mut graph_file = read_graph_file(&dir)?.file;
        guard_version(&dir, graph_file.version)?;
        if let Some(marker) = graph_file.backup {
            return Err(PersistError::IsBackup {
                path: dir.display(),
                commit_seq: marker.commit_seq,
            });
        }
        if let Some(intent) = Intent::read(&dir)? {
            complete_intent(&dir, &intent)?;
            graph_file = read_graph_file(&dir)?.file;
        }
        remove_tmp(&dir)?;
        let name = name.unwrap_or_default();
        // The name's uniqueness is checked once the position is known (the merge does it).
        check_snapshot_name(&[], name)?;
        let write = checkpoint_write_options(&options, graph_file.chunk_bytes, name);
        let merged = if options.checkpoint_merge_bytes == 0 {
            merge::Merged::TooLarge { wal_bytes: 0 }
        } else {
            merge::merge_checkpoint(&merge::MergeRequest {
                dir: &dir,
                graph_file: &graph_file,
                durable: None,
                write: &write,
                read: &options.read,
                max_wal_bytes: options.checkpoint_merge_bytes,
            })?
        };
        match merged {
            merge::Merged::Written { info, stats } => Ok(CheckpointReport::merged(*info, &stats)),
            merge::Merged::Current { commit_seq } => {
                let existing = list_snapshot_infos(&dir)?;
                match existing_snapshot(&existing, commit_seq, name)? {
                    Some(same) => Ok(CheckpointReport::existing(same)),
                    None => Err(PersistError::store(
                        "write a checkpoint",
                        StoreFailure::ChangedConcurrently,
                        format!("the snapshot at commit {commit_seq} vanished"),
                    )),
                }
            }
            merge::Merged::TooLarge { .. } | merge::Merged::NoBase => {
                let recovered = StoreCore::load_at(&dir, RecoveryTarget::Latest, &options.read)?;
                let graph = recovered.graph;
                if let Some(reason) = super::definitions::critical_unknown(graph.definitions.iter())
                {
                    return Err(PersistError::ReadOnly {
                        path: dir.display(),
                        reason,
                    });
                }
                let seq = graph.last_commit_seq();
                let existing = list_snapshot_infos(&dir)?;
                if let Some(same) = existing_snapshot(&existing, seq, name)? {
                    return Ok(CheckpointReport::existing(same));
                }
                check_snapshot_name(&existing, name)?;
                let snapshot = NewSnapshotDir::begin(&dir, seq)?;
                write_graph_snapshot(snapshot.tmp(), &graph, &write)?;
                let (_, mut info) = snapshot.finish()?;
                info.meta.clear();
                Ok(CheckpointReport::encoded(info))
            }
        }
    }

    /// Identity, position and files of the store in `dir`, read from its files without a lock
    /// (works while a writer has it open): the position is the last complete commit in the WAL.
    pub fn inspect(dir: impl IntoStoreDir) -> Result<StoreInfo, PersistError> {
        let dir = &DirPath::root(dir.into_store_dir());
        files::refuse_other_version(dir)?;
        let file = read_graph_file(dir)?.file;
        let mut info = info_of(dir, &file)?;
        let (commit_seq, time) = maintenance::last_commit_in_files(dir, &info.snapshots)?;
        info.commit_seq = commit_seq;
        info.last_commit_at = time;
        (info.definitions_by_kind, info.schema_models) = maintenance::definitions_in_files(dir)?;
        info.definitions = info.definitions_by_kind.values().sum();
        Ok(info)
    }

    /// [`Store::fork`] of the store in `dir`, read without a lock.
    pub fn fork_dir(
        dir: impl IntoStoreDir,
        target: RecoveryTarget,
        new_dir: impl IntoStoreDir,
    ) -> Result<ForkReport, PersistError> {
        StoreCore::fork_at(
            &DirPath::root(dir.into_store_dir()),
            target,
            &DirPath::root(new_dir.into_store_dir()),
        )
    }

    /// [`Store::compaction_advice`] of the store in `dir`, without a lock (works while a writer
    /// has it open).
    pub fn compaction_advice_dir(
        dir: impl IntoStoreDir,
        policy: CompactionPolicy,
    ) -> Result<CompactionAdvice, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_other_version(&dir)?;
        let file = read_graph_file(&dir)?.file;
        let blocked = file.backup.map(|marker| {
            format!(
                "it is a backup (up to commit {}): restore it first",
                marker.commit_seq
            )
        });
        compaction::advise(&dir, policy, blocked)
    }

    /// What [`Store::prune`] (or [`Store::prune_backup`] on a backup directory) up to `up_to`
    /// would remove from the store in `dir`, without removing anything and without a lock:
    /// the same estimate the compaction advice makes (names only, plus the `marks` file when a
    /// snapshot would go). Front ends read its [`PruneReport::marks_unreachable`] to ask before
    /// a prune makes marks unreachable (the CLI's `store prune` refuses without
    /// `--drop-marks` then); the prune itself never asks.
    pub fn prune_estimate_dir(
        dir: impl IntoStoreDir,
        up_to: u64,
    ) -> Result<PruneReport, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_other_version(&dir)?;
        read_graph_file(&dir)?;
        Ok(maintenance::prune_estimate(&dir, up_to)?.0)
    }

    /// [`Store::verify`] of the store in `dir`, without a lock (works while a writer has it
    /// open; an incomplete last record is then a note, not damage).
    pub fn verify_dir(dir: impl IntoStoreDir) -> Result<VerifyReport, PersistError> {
        maintenance::verify(&DirPath::root(dir.into_store_dir()), None)
    }

    /// [`Store::backup`] of the store in `dir` by another process (no durable end there: the
    /// WAL is copied up to its last complete, valid record).
    pub fn backup_dir(
        dir: impl IntoStoreDir,
        target_dir: impl IntoStoreDir,
    ) -> Result<BackupReport, PersistError> {
        Self::backup_dir_with(dir, target_dir, BackupMode::Auto)
    }

    /// [`Store::backup_dir`] with a [`BackupMode`].
    pub fn backup_dir_with(
        dir: impl IntoStoreDir,
        target_dir: impl IntoStoreDir,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError> {
        backup::backup_into(
            &DirPath::root(dir.into_store_dir()),
            None,
            &DirPath::root(target_dir.into_store_dir()),
            mode,
        )
    }

    /// [`Store::backup_zip`] of the store in `dir` by another process.
    #[cfg(feature = "persist-zip")]
    pub fn backup_zip_dir<W>(
        dir: impl IntoStoreDir,
        mut archive: W,
    ) -> Result<BackupReport, PersistError>
    where
        W: std::io::Read + std::io::Write + std::io::Seek + Send,
    {
        maintenance::backup_to_zip(
            &DirPath::root(dir.into_store_dir()),
            None,
            &mut archive,
            ModelCopy::Include,
        )
    }

    /// [`Store::backup_zip_with`] of the store in `dir` by another process.
    #[cfg(feature = "persist-zip")]
    pub fn backup_zip_dir_with<W>(
        dir: impl IntoStoreDir,
        mut archive: W,
        models: ModelCopy,
    ) -> Result<BackupReport, PersistError>
    where
        W: std::io::Read + std::io::Write + std::io::Seek + Send,
    {
        maintenance::backup_to_zip(
            &DirPath::root(dir.into_store_dir()),
            None,
            &mut archive,
            models,
        )
    }

    /// Unpacks a backup ZIP ([`Store::backup_zip`]) into `dir` (which must not exist or be
    /// empty); every entry's CRC is checked. The result is the LIVE store, not a marked backup
    /// (unpacking an archive is the explicit request to restore it): open it with
    /// [`Store::open`]. A ZIP of a store of an older released format (Graphersal 0.1.x) is
    /// upgraded to the current format as it is restored ([`Store::upgrade_in_place`] of the
    /// unpacked store: verified, everything carried); the archive itself is only read.
    #[cfg(feature = "persist-zip")]
    pub fn restore_zip(
        reader: impl std::io::Read,
        dir: impl IntoStoreDir,
    ) -> Result<(), PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        let existed = dir.exists();
        zip::restore(reader, &dir)?;
        // The models' files against their manifests (the archive's CRCs passed already).
        for id in models::model_ids(&dir)? {
            if let Some((path, reason)) = models::check(&dir, &id).1.into_iter().next() {
                files::undo_new_dir(&dir, existed);
                return Err(PersistError::corrupt(
                    format!("backup archive entry {path}"),
                    0,
                    reason,
                ));
            }
        }
        // A backup of a NEWER format (format spec 15): nothing of it is left behind.
        if let Err(err) = files::refuse_newer_version(&dir) {
            files::undo_new_dir(&dir, existed);
            return Err(err);
        }
        let mut file = match read_graph_file(&dir) {
            Ok(read) => read.file,
            // A backup of a released older store format is upgraded through the converter
            // (verified; format spec 15.1); a pre-release one leaves nothing behind.
            Err(PersistError::OlderFormat { found, .. }) if upgradable_from(found) => {
                if let Err(err) = upgrade::upgrade_in_place(&dir) {
                    files::undo_new_dir(&dir, existed);
                    return Err(err);
                }
                read_graph_file(&dir)?.file
            }
            Err(err @ PersistError::OlderFormat { .. }) => {
                files::undo_new_dir(&dir, existed);
                return Err(err);
            }
            Err(err) => return Err(err),
        };
        if file.backup.take().is_some() {
            write_graph_file(&dir, &file)?;
        }
        Ok(())
    }

    /// Makes the backup in `dir` the LIVE store, IN PLACE (the "the original is lost, continue
    /// from the backup" case): its backup marker is cleared, so it opens read-write with
    /// [`Store::open`] and keeps the original's lineage (the same `graph_id`). An interrupted
    /// incremental backup into it is rolled back first. Returns the marker that was cleared.
    /// For an independent writable copy that leaves the backup as it is, fork it
    /// ([`Store::fork_dir`]) instead.
    pub fn restore_backup(dir: impl IntoStoreDir) -> Result<BackupMarker, PersistError> {
        backup::restore(&DirPath::root(dir.into_store_dir()))
    }

    /// [`Store::prune`] of the backup in `dir` (a backup has its own retention, by the same
    /// rules): takes the backup directory's writer lock; an interrupted increment is rolled
    /// back first. Fails on a store that is not a backup (open it and prune it).
    pub fn prune_backup(dir: impl IntoStoreDir, up_to: u64) -> Result<PruneReport, PersistError> {
        backup::prune(&DirPath::root(dir.into_store_dir()), up_to)
    }

    /// The backup marker of the directory `dir` (`None`: a store, not a backup).
    pub fn backup_marker_dir(dir: impl IntoStoreDir) -> Result<Option<BackupMarker>, PersistError> {
        backup::marker_of(&DirPath::root(dir.into_store_dir()))
    }

    /// Opens the BACKUP in `dir` READ-ONLY as a [`Store`] (its latest state): listing,
    /// [`Store::info`], [`Store::read_only_at`] (any commit it holds), [`Store::fork`],
    /// [`Store::verify`], [`Store::export_packed`] and backups of it work; every commit of
    /// [`Store::graph`], marks, checkpoints, prune and rollback are refused with
    /// [`PersistError::IsBackup`]. Takes no lock and writes nothing (an incremental backup may
    /// continue it meanwhile; this store keeps the state it loaded). Fails on a directory that
    /// is not a backup ([`Store::open`] opens those).
    pub fn open_backup(
        dir: impl IntoStoreDir,
        options: StoreOptions,
    ) -> Result<Store, PersistError> {
        Self::from_core(StoreCore::open_backup_at(
            DirPath::root(dir.into_store_dir()),
            options,
            &TraversalGraphs,
        )?)
    }

    /// [`Store::attic_list`] of the store in `dir`, without a lock.
    pub fn attic_list_dir(dir: impl IntoStoreDir) -> Result<Vec<AtticEntry>, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_older_format(&dir)?;
        rollback::attic_list(&dir)
    }

    /// [`Store::attic_changes`] of the store in `dir`, without a lock.
    pub fn attic_changes_dir(
        dir: impl IntoStoreDir,
        id: &str,
    ) -> Result<ChangesSince, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_older_format(&dir)?;
        StoreCore::attic_changes_at(&dir, id)
    }

    /// [`Store::attic_fork`] of the store in `dir`, without a lock.
    pub fn attic_fork_dir(
        dir: impl IntoStoreDir,
        id: &str,
        new_dir: impl IntoStoreDir,
    ) -> Result<ForkReport, PersistError> {
        rollback::attic_fork(
            &DirPath::root(dir.into_store_dir()),
            id,
            &DirPath::root(new_dir.into_store_dir()),
        )
    }
}

impl<S: PersistentStorage + Default> Store<S> {
    /// The `Store` of an opened core: the core's graph, typed.
    fn from_core(core: StoreCore) -> Result<Store<S>, PersistError> {
        let graph = core
            .shared
            .graph
            .clone()
            .into_any()
            .downcast::<Graph<S>>()
            .map_err(|_| {
                PersistError::Graph(crate::error::GraphError::unsupported(
                    "a store whose graph is not of the store's storage type",
                ))
            })?;
        // The lock's wait and hold times go to the store's workload statistics.
        graph.attach_workload_stats(Arc::clone(&core.shared.workload));
        Ok(Store {
            core: Arc::new(core),
            graph,
        })
    }

    /// Opens the store in `dir` read-write as [`Store::open`] does, with the graph held by a
    /// storage of the host's type `S` (EXPERIMENTAL, like [`PersistentStorage`]): `empty`
    /// builds the empty instance the store is loaded into (through the storage's load sink,
    /// then the WAL replayed by [`PersistentStorage::replay`]). The files are the same for
    /// every storage type, so this opens a store written as `Store<TraversalGraph>` too (and
    /// [`Store::open`] opens one written here).
    ///
    /// The storage must claim `atomic` and `change_capture`
    /// ([`StorageCapabilities`](crate::graph::StorageCapabilities)): the journal writes its
    /// record in `before_commit` and must be able to veto. Without them the open fails with
    /// [`PersistError::Store`] of cause [`StoreFailure::MissingCapability`] naming the missing
    /// one. A store that opens in maintenance mode (damage, [`Store::maintenance`]) holds the
    /// salvaged state copied into a new `S` from `empty`.
    pub fn open_in(
        dir: impl IntoStoreDir,
        options: StoreOptions,
        empty: impl Fn() -> S + Send + Sync + 'static,
    ) -> Result<Store<S>, PersistError> {
        Self::from_core(StoreCore::open_at(
            DirPath::root(dir.into_store_dir()),
            options,
            &graph::Storages(empty),
        )?)
    }

    /// Creates a store whose first snapshot is `graph`, a storage of the host's type `S`
    /// (its data, schema, catalog, lineage id, commit position and auto-id sequences; written
    /// through the read API), and opens it as [`Store::open_in`] does with `S::default` as
    /// the empty instance. `dir` must not exist or be empty. The capability precondition of
    /// [`Store::open_in`] is checked BEFORE anything is written.
    pub fn create_in(
        dir: impl IntoStoreDir,
        graph: S,
        options: StoreOptions,
    ) -> Result<Store<S>, PersistError> {
        graph::require_journal_capabilities(graph.capabilities()?)?;
        let dir = DirPath::root(dir.into_store_dir());
        let lineage = vec![LineageEntry {
            graph_id: graph.graph_id(),
            branched_at: 0,
            time: now(),
        }];
        StoreCore::create_lineage(&dir, &graph, lineage, WriteOptions::new(), &options)?;
        drop(graph);
        Self::from_core(StoreCore::open_at(
            dir,
            options,
            &graph::Storages(S::default),
        )?)
    }

    /// Creates a store from a packed snapshot (`.gsnap`) as [`Store::create_from_packed`]
    /// does, and opens it as [`Store::open_in`] does.
    pub fn create_from_packed_in(
        dir: impl IntoStoreDir,
        packed: impl std::io::Read,
        options: StoreOptions,
        empty: impl Fn() -> S + Send + Sync + 'static,
    ) -> Result<Store<S>, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        StoreCore::create_from_packed_at(&dir, packed, &options)?;
        Self::from_core(StoreCore::open_at(dir, options, &graph::Storages(empty))?)
    }

    /// Opens the BACKUP in `dir` read-only as [`Store::open_backup`] does, with the graph held
    /// by a storage of the host's type `S` built by `empty` (see [`Store::open_in`]).
    pub fn open_backup_in(
        dir: impl IntoStoreDir,
        options: StoreOptions,
        empty: impl Fn() -> S + Send + Sync + 'static,
    ) -> Result<Store<S>, PersistError> {
        Self::from_core(StoreCore::open_backup_at(
            DirPath::root(dir.into_store_dir()),
            options,
            &graph::Storages(empty),
        )?)
    }

    /// Loads the store in `dir` read-only at `target` as [`Store::open_read_only`] does, into
    /// `empty`, an empty storage of the host's type `S` (no lock, no file changed; the graph
    /// is unpublished and without a journal).
    pub fn open_read_only_in(
        dir: impl IntoStoreDir,
        target: RecoveryTarget,
        empty: S,
    ) -> Result<Recovered<S>, PersistError> {
        StoreCore::load_plan_at(
            &DirPath::root(dir.into_store_dir()),
            target,
            &ReadOptions::default(),
        )?
        .run_in(empty)
    }

    /// Why the store is read-only because it holds something critical this version does not
    /// know (format spec 16 rule 3: a critical definition of an unknown kind, a snapshot file of
    /// a critical unknown segment kind): commits, marks, checkpoints, prune and rollback are
    /// refused with [`PersistError::ReadOnly`]; reading, [`Store::fork`], [`Store::backup`] and
    /// [`Store::verify`] work and keep it. `None`: not for that reason.
    ///
    /// Fails with [`StoreFailure::LockPoisoned`] when a thread panicked while it held the
    /// store's lock of that reason (the answer would be a guess).
    pub fn read_only_reason(&self) -> Result<Option<String>, PersistError> {
        self.core.read_only_reason()
    }

    /// The damage report when the store opened in maintenance mode (format spec 13): damage
    /// was found, so it is READ-ONLY (commits, marks, checkpoints, prune and rollback are
    /// refused with [`PersistError::Maintenance`]). Read what is intact; repair with
    /// [`Store::repair_to`]. `None`: the store is healthy and writable.
    pub fn maintenance(&self) -> Option<&DamageReport> {
        self.core.maintenance()
    }

    /// Builds a repaired copy of this store in `new_dir` (format spec 13; never in place: a
    /// failing disk gets no more writes): damaged snapshot chunks are rebuilt from donors
    /// (an older snapshot's chunks for the id range plus the WAL for it), damaged WAL records
    /// a later snapshot covers are skipped, otherwise the replay continues after the gap and
    /// every element whose state differs from a later record's before image is reported as
    /// diverged. The new store has a new lineage whose parent is this one; it is verified
    /// before this returns. The damaged files are never changed or deleted. Works on a healthy
    /// store too (a fresh copy). Marks are not carried (the new lineage starts without any):
    /// [`RepairReport::marks_not_carried`] lists them. The repair works on a `TraversalGraph`
    /// of its own whatever this store's storage type; the files it writes are
    /// storage-independent, so a host opens the new store with [`Store::open_in`].
    pub fn repair_to(&self, new_dir: impl IntoStoreDir) -> Result<RepairReport, PersistError> {
        self.core.repair_to(new_dir)
    }

    /// [`Store::open_read_only`] of this store's directory into a new `S::default()` (no lock
    /// is taken; the open store is not changed).
    pub fn read_only_at(&self, target: RecoveryTarget) -> Result<Recovered<S>, PersistError> {
        self.core.read_only_at(target, S::default())
    }

    /// What the open did (clean close, recovery, torn tail cut, ...).
    pub fn open_report(&self) -> &OpenReport {
        self.core.open_report()
    }

    /// The graph. Every commit of it goes to the WAL before it returns.
    pub fn graph(&self) -> &Arc<Graph<S>> {
        &self.graph
    }

    /// The store directory.
    pub fn dir(&self) -> &Arc<dyn StoreDir> {
        self.core.dir()
    }

    /// Where the store is, for messages ([`StoreDir::location`]: a path, `memory:main`).
    pub fn location(&self) -> String {
        self.core.location()
    }

    /// The durable end of the WAL (lock-free).
    pub fn durable_end(&self) -> DurablePosition {
        self.core.durable_end()
    }

    /// The error of the last automatic checkpoint, if it failed; `None` again after one
    /// succeeded. A failed automatic checkpoint is retried at the next commit while the WAL since
    /// the last snapshot is still at or above [`StoreOptions::with_checkpoint_after_bytes`]
    /// (at most one attempt per commit, never in a loop of its own; on wasm32 at the next
    /// [`Store::run_due_checkpoint`]). A manual [`Store::checkpoint`] reports its own error.
    /// The error is the checkpoint's own [`PersistError`] (shared: it stays readable until the
    /// next automatic checkpoint). Fails with [`StoreFailure::LockPoisoned`] when a thread
    /// panicked while it held the lock of that error.
    pub fn last_auto_checkpoint_error(&self) -> Result<Option<Arc<PersistError>>, PersistError> {
        self.core.last_auto_checkpoint_error()
    }

    /// Writes a snapshot of the current state (format spec 12.3): MERGED from the newest
    /// snapshot and the WAL after it, without the graph's lock (commits go on meanwhile; the
    /// journal is synced first when the durable end lags the last commit), with memory
    /// proportional to that WAL; segments no commit touched are reused by reference (an
    /// incremental snapshot). Written to `snapshots/tmp-*`, synced, verified, renamed; then a
    /// Checkpoint record goes to the WAL and the next record starts a new WAL segment. Beyond
    /// [`StoreOptions::with_checkpoint_merge_bytes`] the loaded graph is encoded under its read
    /// lock instead. Damage in what the merge reads fails the checkpoint with nothing written.
    ///
    /// A `name` (at most 255 bytes) must be unique in the store. When a snapshot at the current
    /// commit already exists, it is returned unchanged (and a different `name` is an error).
    pub fn checkpoint(&self, name: Option<&str>) -> Result<SnapshotInfo, PersistError> {
        self.core.checkpoint(name)
    }

    /// Sets a mark at the current position ([`GraphStorage::mark`](crate::graph::GraphStorage::mark)); names are unique along
    /// the store's whole history (the `marks` file), also the names of marks a prune made
    /// unreachable.
    pub fn mark(&self, name: &str) -> Result<Mark, PersistError> {
        self.core.mark(name)
    }

    /// The snapshots, oldest first, from their manifests' fixed parts (`meta` is not read).
    pub fn snapshots(&self) -> Result<Vec<SnapshotInfo>, PersistError> {
        self.core.snapshots()
    }

    /// The marks a target can reach, in order (the `marks` file; rebuilt from the WAL when it
    /// is missing): a mark before the oldest snapshot is left out, a prune removed the history
    /// it needs ([`PruneReport::marks_unreachable`] names them).
    pub fn marks(&self) -> Result<Vec<Mark>, PersistError> {
        self.core.marks()
    }

    /// Identity, position and files of this open store.
    pub fn info(&self) -> Result<StoreInfo, PersistError> {
        self.core.info()
    }

    /// Writes the packed snapshot (`.gsnap`) of the stored snapshot at `commit_seq` (`None`:
    /// the latest) to `writer` by copying its files, without any graph lock.
    pub fn export_packed(
        &self,
        commit_seq: Option<u64>,
        writer: impl std::io::Write,
    ) -> Result<SnapshotInfo, PersistError> {
        self.core
            .export_packed(commit_seq, writer, ModelCopy::Include)
    }

    /// [`Store::export_packed`] that carries the store's embedding models
    /// ([`ModelCopy::Include`], what [`Store::export_packed`] does) or leaves them out
    /// ([`ModelCopy::Exclude`]). The models travel as named extra files of an ignorable kind
    /// (format spec 4.5, 8): a Graphersal without models (0.3.0) loads such a snapshot and
    /// ignores them. Every model file is checked against its manifest first; a damaged one
    /// stops the export (export without models then).
    pub fn export_packed_with(
        &self,
        commit_seq: Option<u64>,
        writer: impl std::io::Write,
        models: ModelCopy,
    ) -> Result<SnapshotInfo, PersistError> {
        self.core.export_packed(commit_seq, writer, models)
    }

    /// The embedding models this store keeps (format spec 4.5): identity, origin, licence and
    /// files of each, from their manifests (nothing else is read; [`Store::verify`] checks the
    /// files). Book page "Embedding Models in a Store".
    pub fn models(&self) -> Result<Vec<StoredModel>, PersistError> {
        models::list(&self.core.shared.dir)
    }

    /// Adds an embedding model to this open store (the explicit command a model enters a store
    /// by, format spec 4.5): its files are streamed into `models/<id>/` with their SHA-256,
    /// the standard licence text is written when the model has no licence file and its
    /// licence is MIT or Apache-2.0, the manifest last; the directory appears at once (a
    /// rename). The same model stored already is not written again
    /// ([`AddModelReport::already_stored`]). The files are checked for their roles and names,
    /// not loaded: a host that can load the model (feature `model2vec`) checks it first.
    /// Refused on a read-only store (a backup, maintenance mode, ...).
    pub fn add_model(&self, model: NewModel) -> Result<AddModelReport, PersistError> {
        self.core.writable("add a model")?;
        let (model, added) = models::add(&self.core.shared.dir, model, models::now_text())?;
        Ok(AddModelReport {
            model,
            already_stored: !added,
        })
    }

    /// Removes the embedding model `key` names (its identity `model2vec/<repo>@<12 hex>` or
    /// its id `model2vec-<12 hex>`) from this open store: the only way a model leaves a store.
    /// The schema keeps its `embedder` identity; a later open notes the missing model. Refused
    /// while a backup copies the store's files, and on a read-only store.
    pub fn remove_model(&self, key: &str) -> Result<StoredModel, PersistError> {
        self.core.writable("remove a model")?;
        remove_model_at(&self.core.shared.dir, key)
    }

    /// Puts the embedding models a packed snapshot carries (format spec 4.5, 8) into this open
    /// store, leaving its graph alone: a host that loaded a `.gsnap` into a new store through
    /// its graph ([`Store::create_with`]) keeps the file's models this way
    /// ([`Store::create_from_packed`] does it itself). Every model file is checked against its
    /// manifest; a model the store holds already stays as it is. Returns the models of the
    /// file. Refused on a read-only store.
    pub fn import_models_from_packed(
        &self,
        packed: impl std::io::Read,
    ) -> Result<Vec<StoredModel>, PersistError> {
        self.core.writable("add the models of a packed snapshot")?;
        snapshot_dir::import_packed_models(&self.core.shared.dir, packed)
    }

    /// The bytes of the file `name` of the stored model `key` names (identity or id), checked
    /// against the model's manifest (size and SHA-256).
    pub fn read_model_file(&self, key: &str, name: &str) -> Result<Vec<u8>, PersistError> {
        read_model_file_at(&self.core.shared.dir, key, name)
    }

    /// Branches the state at `target` into a NEW store in `new_dir` (format spec 12.5): a new
    /// lineage id with this store's as parent, its first snapshot at the target. This store is
    /// not changed. Marks are not carried (the new lineage starts without any):
    /// [`ForkReport::marks_not_carried`] lists the ones of the forked history.
    pub fn fork(
        &self,
        target: RecoveryTarget,
        new_dir: impl IntoStoreDir,
    ) -> Result<ForkReport, PersistError> {
        self.core.fork(target, new_dir)
    }

    /// Removes snapshots and WAL that only serve targets before `up_to` (an explicit bound:
    /// nothing is ever deleted implicitly). Kept: the newest snapshot at or before `up_to`,
    /// everything after it, and always the last two snapshots and the WAL between them (donor
    /// retention, format spec 12.4); the kept snapshots are verified before anything is deleted.
    /// Fails while a backup copies the store's files. Runs under an `INTENT`, so an
    /// interruption is completed at the next open.
    pub fn prune(&self, up_to: u64) -> Result<PruneReport, PersistError> {
        self.core.prune(up_to)
    }

    /// Gives disk space back: [`Store::prune`] up to the current commit (the last two
    /// snapshots, the WAL between them and every attic base stay), then the backend's
    /// compaction ([`StoreDir::compact`]): a single-file store copies its live data into a new
    /// file next to it and replaces the old one atomically (the disk needs room for the live
    /// data meanwhile; commits wait while it runs). For a directory store the prune is all
    /// (its removals free the space at once). [`Store::compaction_advice`] says whether it is
    /// worth it.
    pub fn compact(&self) -> Result<CompactReport, PersistError> {
        self.core.compact()
    }

    /// Whether [`Store::compact`] is worth it now, by the options' [`CompactionPolicy`]
    /// ([`StoreOptions::with_compaction_policy`]), with the numbers behind it: the file's
    /// total and live bytes, its garbage, what a prune would free first, the free disk space
    /// a compaction needs. Cheap: the backend's bookkeeping and the names and lengths of the
    /// store's files, no data is read; safe to poll from any thread while the store is in use.
    /// Never recommended while the store cannot change (maintenance mode, a backup, closed)
    /// or when the disk lacks the space.
    pub fn compaction_advice(&self) -> Result<CompactionAdvice, PersistError> {
        self.core.compaction_advice()
    }

    /// [`Store::compaction_advice`] by another policy.
    pub fn compaction_advice_with(
        &self,
        policy: CompactionPolicy,
    ) -> Result<CompactionAdvice, PersistError> {
        self.core.compaction_advice_with(policy)
    }

    /// Scrubs the whole store: every checksum of `GRAPH`, every snapshot (fully decoded: ids
    /// in order, ranges), every WAL record, commit continuity, lineage, the `marks` file
    /// (format spec 12.9). Run it regularly: it reports damage while donors still exist.
    ///
    /// Damage it finds in an open store is damage found while the store is open: with the
    /// damage policy `maintenance` the store turns read-only ([`Store::damage_found`]).
    pub fn verify(&self) -> Result<VerifyReport, PersistError> {
        self.core.verify()
    }

    /// What the store does when damage is found on disk while it is open (format spec 5.2
    /// bit 5), fixed when the store was created ([`StoreOptions::with_damage_policy`]). Fails
    /// with [`StoreFailure::LockPoisoned`] when a thread panicked while it held the lock of the
    /// store's `GRAPH` file state.
    pub fn damage_policy(&self) -> Result<DamagePolicy, PersistError> {
        self.core.damage_policy()
    }

    /// The store's identity ([`StoreInfo::store_id`], format spec 5.1), fixed when the store
    /// was created; a fork, repair or conversion gets a new one. Fails like
    /// [`Self::damage_policy`].
    pub fn store_id(&self) -> Result<u128, PersistError> {
        self.core.store_id()
    }

    /// The damage found on disk since the store was opened (by [`Store::backup`],
    /// [`Store::checkpoint`] and the automatic checkpoint, [`Store::verify`]), in the order it
    /// was found; empty while none was. It stays reported for as long as the store is open.
    /// With the damage policy `maintenance` the first finding made the store read-only
    /// ([`PersistError::DamageFound`]); save its graph with [`Store::backup_from_memory`] and
    /// repair the store ([`Store::repair_dir`]). Damage found when the store was OPENED is
    /// [`Store::maintenance`] instead. Fails with [`StoreFailure::LockPoisoned`] when a thread
    /// panicked while it held the lock of that list.
    pub fn damage_found(&self) -> Result<Vec<FoundDamage>, PersistError> {
        self.core.damage_found()
    }

    /// Whether the store turned read-only because damage was found on disk while it is open
    /// (its damage policy `maintenance`).
    pub fn is_frozen_by_damage(&self) -> bool {
        self.core.is_frozen_by_damage()
    }

    /// Why the store stopped: an in-place [`Store::rollback_to`] or [`Store::attic_restore`]
    /// failed part-way (its files changed, or loading the new state failed; then the graph in
    /// memory was emptied, [`StoreFailure::ReloadFailed`]). A stopped store writes nothing more:
    /// commits, marks, checkpoints, prune, rollback and restore are refused
    /// ([`StoreFailure::Stopped`]) and closing it writes no `GRAPH`. Close it and open it again:
    /// the open completes the change from its `INTENT` (or undoes it). `None`: not stopped.
    /// Fails with [`StoreFailure::LockPoisoned`] when a thread panicked while it held the lock
    /// of that reason.
    pub fn stopped_reason(&self) -> Result<Option<String>, PersistError> {
        self.core.stopped_reason()
    }

    /// A backup of the store into `target_dir` (format spec 12.6, 12.7), without any graph lock
    /// ([`BackupMode::Auto`]): a FULL backup into an empty or missing directory (the latest
    /// snapshot and the WAL after it up to the journal's durable end), an INCREMENT into a
    /// backup directory of this store (only the snapshots and WAL it does not hold yet: new
    /// snapshots, new WAL segments, and the tail of its last segment up to the durable end,
    /// after checking that the backup's WAL is a prefix of the store's). The store's files
    /// are pinned against `prune` while they are copied (but WAL the backup still needs is not
    /// kept for it: when the store pruned it, the increment is refused and a full backup is
    /// needed).
    ///
    /// The backup keeps the store's lineage and is MARKED in its `GRAPH` file: it opens
    /// read-only ([`Store::open_backup`], [`Store::open_read_only`] at any commit it holds);
    /// [`Store::restore_backup`] makes it the live store. An increment runs under an `INTENT`
    /// in the backup directory: interrupted, it is rolled back at the next backup, restore or
    /// prune of it, and the backup stays a valid store after every step. It follows an
    /// in-place rollback of the store whose target is at or after the backup's position; a
    /// backup of another graph, or one a rollback went behind, is refused.
    pub fn backup(&self, target_dir: impl IntoStoreDir) -> Result<BackupReport, PersistError> {
        self.core.backup(target_dir)
    }

    /// [`Store::backup`] with a [`BackupMode`] ([`BackupMode::Full`]: always a full backup into
    /// an empty or missing directory).
    pub fn backup_with(
        &self,
        target_dir: impl IntoStoreDir,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError> {
        self.core.backup_with(target_dir, mode)
    }

    /// A backup FROM MEMORY (format spec 12.11): only when the store on disk is damaged
    /// ([`Store::damage_found`] is not empty: a backup, a checkpoint or a verification found
    /// damage while the store is open), its graph in memory is written straight into
    /// `target_dir` by the streaming encoder under the graph's read lock (commits wait): one
    /// snapshot at the graph's commit, `GRAPH` with the backup marker and the store's identity,
    /// lineage and creation parameters, an empty WAL segment after that commit. NOTHING is read
    /// from or written to the store's own directory (a damaged disk gets no more writes).
    ///
    /// Into an empty or missing directory: a full backup. Into a backup of this store
    /// ([`BackupMode::Auto`]): an increment (its snapshot is newer than everything the backup
    /// holds, under an `INTENT` rolled back on failure); a later normal increment of that
    /// backup then needs a full backup and says so. The result is an ordinary marked backup:
    /// [`Store::restore_backup`] makes it the live store, [`Store::fork_dir`] a new one. Marks
    /// are not carried (the backup holds no WAL before its snapshot, like a fork).
    ///
    /// Refused while no damage was found (a normal [`Store::backup`] checks every checksum and
    /// is the right tool then), for a store opened in maintenance mode (its graph is the state
    /// salvaged from donors, not a verified one: [`Store::repair_to`] writes it) and for a
    /// backup opened read-only.
    pub fn backup_from_memory(
        &self,
        target_dir: impl IntoStoreDir,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError> {
        self.core.backup_from_memory(target_dir, mode)
    }

    /// A full backup as one ZIP archive written to `writer` (stored entries, zip64; feature
    /// `persist-zip`). ZIP backups are always full; the `GRAPH` file inside carries the backup
    /// marker, which [`Store::restore_zip`] clears.
    ///
    /// The archive is written from its current position on and then READ BACK through the
    /// same handle (hence `Read + Seek`: a file opened for reading and writing, a
    /// `Cursor<Vec<u8>>`): every entry's CRC-32 and every checksum of the store files in it.
    /// The store's files are checked before they are copied; damage on either side fails the
    /// backup with [`PersistError::BackupDamaged`] (the caller removes the archive).
    #[cfg(feature = "persist-zip")]
    pub fn backup_zip<W>(&self, archive: W) -> Result<BackupReport, PersistError>
    where
        W: std::io::Read + std::io::Write + std::io::Seek + Send,
    {
        self.core.backup_zip(archive, ModelCopy::Include)
    }

    /// [`Store::backup_zip`] that carries the store's embedding models ([`ModelCopy::Include`],
    /// what [`Store::backup_zip`] does) or leaves them out ([`ModelCopy::Exclude`]: an archive
    /// to hand on without the models, which Graphersal 0.3.0 also restores; one with models
    /// it refuses as written by a newer version). Every model file is checked against its
    /// manifest in the store and again in the archive.
    #[cfg(feature = "persist-zip")]
    pub fn backup_zip_with<W>(
        &self,
        archive: W,
        models: ModelCopy,
    ) -> Result<BackupReport, PersistError>
    where
        W: std::io::Read + std::io::Write + std::io::Seek + Send,
    {
        self.core.backup_zip(archive, models)
    }

    /// The backup marker when this store is a backup opened read-only ([`Store::open_backup`]).
    pub fn backup_marker(&self) -> Option<BackupMarker> {
        self.core.backup_marker()
    }

    /// The committed change sets after `commit_seq`, in order, read from the WAL up to its
    /// durable end (CDC, replication, audit). Fails when the WAL after `commit_seq` was pruned.
    pub fn changes_since(&self, commit_seq: u64) -> Result<ChangesSince, PersistError> {
        self.core.changes_since(commit_seq)
    }

    /// Rolls the store back IN PLACE to `target` (format spec 12.5): everything after it (later
    /// snapshots, the WAL after it; the WAL segment holding the target is split) moves into
    /// the attic ([`RollbackMode::Move`]) or is deleted ([`RollbackMode::Delete`]); the store
    /// continues as a new lineage (a new `graph_id` whose parent is the old one at the target)
    /// with a new WAL segment. Runs under an `INTENT`, so an interruption is completed at the
    /// next open; commits wait meanwhile. The graph of [`Store::graph`] is replaced in place
    /// (commit hooks the application registered on it are kept).
    ///
    /// Fails when the target is not before the current commit, when no snapshot at or before
    /// it is left, or when the WAL after it cannot be read.
    pub fn rollback_to(
        &self,
        target: RecoveryTarget,
        mode: RollbackMode,
    ) -> Result<RollbackReport, PersistError> {
        self.core.rollback_to(target, mode)
    }

    /// The attic entries (history moved aside by [`Store::rollback_to`]), oldest first.
    pub fn attic_list(&self) -> Result<Vec<AtticEntry>, PersistError> {
        self.core.attic_list()
    }

    /// The committed change sets of attic entry `id`, in order (who, when, what was rolled
    /// back).
    pub fn attic_changes(&self, id: &str) -> Result<ChangesSince, PersistError> {
        self.core.attic_changes(id)
    }

    /// Puts attic entry `id` back: undoes its rollback (the moved snapshots and WAL return,
    /// the rollback's lineage is dropped). Only while nothing was committed and no mark was
    /// set since that rollback; otherwise fork the entry ([`Store::attic_fork`]). Returns the
    /// commit the store is at afterwards.
    pub fn attic_restore(&self, id: &str) -> Result<u64, PersistError> {
        self.core.attic_restore(id)
    }

    /// Whether [`Store::attic_restore`] of entry `id` would be allowed now, without changing
    /// anything: `Ok(())`, or the error the restore would fail with (new commits or marks since
    /// that rollback, another rollback since, maintenance mode, no such entry). A UI offers
    /// the restore only when this is `Ok` and the fork ([`Store::attic_fork`]) otherwise.
    pub fn attic_can_restore(&self, id: &str) -> Result<(), PersistError> {
        self.core.attic_can_restore(id)
    }

    /// A new store in `new_dir` from the end of attic entry `id` (the rolled-back history),
    /// with a new lineage whose parent is the entry's lineage. This store is not changed. Marks
    /// are not carried: [`ForkReport::marks_not_carried`] lists the ones of the entry's history.
    pub fn attic_fork(
        &self,
        id: &str,
        new_dir: impl IntoStoreDir,
    ) -> Result<ForkReport, PersistError> {
        self.core.attic_fork(id, new_dir)
    }

    /// Deletes attic entry `id` for good (then [`Store::prune`] may remove the snapshot it
    /// needed).
    pub fn attic_remove(&self, id: &str) -> Result<(), PersistError> {
        self.core.attic_remove(id)
    }

    /// Closes the store: an optional checkpoint, the WAL synced, the store marked closed
    /// cleanly, the lock released. The graph refuses commits from then on.
    pub fn close(self) -> Result<(), PersistError> {
        self.core.shutdown()
    }

    /// [`Store::close`] through a shared reference, for a store other threads still hold (a
    /// server's request threads, a signal handler stopping the process): waits for a running
    /// commit, then closes. A second call (and the drop) does nothing.
    pub fn shutdown(&self) -> Result<(), PersistError> {
        self.core.shutdown()
    }

    /// Runs the automatic checkpoint when it is due (the WAL since the last snapshot reached
    /// [`StoreOptions::with_checkpoint_after_bytes`]) and no background thread runs it: on
    /// wasm32 (no threads) the host calls this after a run that committed. Returns the
    /// snapshot written, `None` when none was due (or a thread takes care of it).
    pub fn run_due_checkpoint(&self) -> Result<Option<SnapshotInfo>, PersistError> {
        self.core.run_due_checkpoint()
    }

    /// The WAL bytes written since the last snapshot (the automatic checkpoint's measure).
    pub fn wal_bytes_since_checkpoint(&self) -> u64 {
        self.core.wal_bytes_since_checkpoint()
    }
}

impl StoreCore {
    /// [`Store::open_backup`] with the graph `maker` builds.
    fn open_backup_at(
        dir: DirPath,
        options: StoreOptions,
        maker: &dyn GraphMaker,
    ) -> Result<StoreCore, PersistError> {
        maker.check()?;
        files::refuse_other_version(&dir)?;
        let graph_file = read_graph_file(&dir)?.file;
        guard_version(&dir, graph_file.version)?;
        let Some(marker) = graph_file.backup else {
            return Err(PersistError::NotAStore {
                path: dir.display(),
                reason: "it is a store, not a backup (it has no backup marker): open it with \
                         Store::open"
                    .into(),
            });
        };
        let plan = Self::load_plan_at(&dir, RecoveryTarget::Latest, &options.read)?;
        let graph = maker.empty();
        let recovered = {
            let mut live = graph.write();
            let recovered = live.reload(&plan)?;
            live.register_guard_hook(Box::new(BackupHook {
                path: dir.display(),
                commit_seq: marker.commit_seq,
            }))?;
            recovered
        };
        let report = OpenReport {
            clean_close: true,
            snapshot_commit_seq: recovered.snapshot_commit_seq,
            commit_seq: recovered.commit_seq,
            commits_replayed: recovered.commits_replayed,
            ..OpenReport::default()
        };
        let wal = Arc::new(WalShared::default());
        wal.closed.store(true, Ordering::Release);
        let workload_stats = workload::open_stats(&dir, options.workload_level, false);
        let shared = Arc::new(Shared {
            chunk_bytes: graph_file.chunk_bytes,
            dir,
            graph,
            journal: JournalCell::default(),
            options,
            graph_file: Arc::new(Mutex::new(graph_file)),
            wal,
            durable: DurableEnd::default(),
            maintenance: Mutex::new(()),
            shutdown: AtomicBool::new(false),
            last_auto_error: Mutex::new(None),
            read_only: Mutex::new(None),
            stopped: Mutex::new(None),
            found: Mutex::new(Vec::new()),
            workload: workload_stats,
        });
        Ok(StoreCore {
            shared,
            report,
            maintenance: None,
            backup: Some(marker),
            lock: Mutex::new(None),
            thread: Mutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    /// The files of [`Store::create_from_packed`] (the store is not opened).
    fn create_from_packed_at(
        dir: &DirPath,
        packed: impl std::io::Read,
        options: &StoreOptions,
    ) -> Result<(), PersistError> {
        let existed = dir.exists();
        prepare_new_dir(dir)?;
        let result = (|| {
            let (_, fixed) = snapshot_dir::import_packed(dir, packed)?;
            write_graph_file(
                dir,
                &GraphFile {
                    version: STORE_VERSION,
                    clean: true,
                    graph_id: fixed.graph_id,
                    created_at: now(),
                    chunk_bytes: fixed.chunk_bytes.max(1),
                    flags: 0,
                    wal_cut: None,
                    wal_head: None,
                    closed_at: None,
                    backup: None,
                    damage_policy: options.damage_policy,
                    // A new store: a new identity (a fork, attic fork and repair come here
                    // too).
                    store_id: crate::value::uuid::new_v7(),
                    lineage: vec![LineageEntry {
                        graph_id: fixed.graph_id,
                        branched_at: 0,
                        time: now(),
                    }],
                },
            )
        })();
        if let Err(err) = result {
            undo_create(dir, existed);
            return Err(err);
        }
        Ok(())
    }

    fn create_lineage<S: PersistentStorage + ?Sized>(
        dir: &DirPath,
        graph: &S,
        lineage: Vec<LineageEntry>,
        write: WriteOptions,
        options: &StoreOptions,
    ) -> Result<(), PersistError> {
        let existed = dir.exists();
        prepare_new_dir(dir)?;
        let result = (|| {
            let write = write
                .with_codec(options.codec)
                .with_chunk_bytes(options.chunk_bytes)
                .with_segment_bytes(options.snapshot_segment_bytes);
            let snapshot = NewSnapshotDir::begin(dir, graph.last_commit_seq()?)?;
            write_graph_snapshot(snapshot.tmp(), graph, &write)?;
            snapshot.finish()?;
            write_graph_file(
                dir,
                &GraphFile {
                    version: STORE_VERSION,
                    clean: true,
                    graph_id: graph.graph_id(),
                    created_at: now(),
                    chunk_bytes: options.chunk_bytes,
                    flags: 0,
                    wal_cut: None,
                    wal_head: None,
                    closed_at: None,
                    backup: None,
                    damage_policy: options.damage_policy,
                    // A new store: a new identity (a fork, attic fork and repair come here
                    // too).
                    store_id: crate::value::uuid::new_v7(),
                    lineage,
                },
            )
        })();
        if result.is_err() {
            undo_create(dir, existed);
        }
        result
    }

    /// [`Store::open`] with the graph `maker` builds.
    fn open_at(
        dir: DirPath,
        options: StoreOptions,
        maker: &dyn GraphMaker,
    ) -> Result<StoreCore, PersistError> {
        maker.check()?;
        // A newer (or older) format is refused before the lock and before any file is read
        // as this version's (format spec 15): never damage, never maintenance mode.
        files::refuse_other_version(&dir)?;
        if !dir.join(files::GRAPH_FILE).exists() && !dir.join(files::GRAPH_COPY).exists() {
            return Err(PersistError::NotAStore {
                path: dir.display(),
                reason: "it has no GRAPH file".into(),
            });
        }
        let lock = DirLock::exclusive(&dir)?;
        // The backend's own records (a single-file container) come before every store file.
        let backend = crate::persist::dir::backend_check(&**dir.store_dir());
        if !backend.damage.is_empty() {
            let trigger = PersistError::corrupt(
                dir.display(),
                0,
                format!("the container is damaged: {}", backend.damage.join("; ")),
            );
            let report = OpenReport {
                warnings: backend.warnings,
                ..OpenReport::default()
            };
            return Self::open_in_maintenance(dir, lock, options, report, trigger, maker);
        }
        let read = match read_graph_file(&dir) {
            Ok(read) => read,
            // A store of an older format is not damage.
            Err(err @ PersistError::OlderFormat { .. }) => return Err(err),
            // Both copies exist but neither is valid: damage (format spec 13).
            Err(err) => {
                return Self::open_in_maintenance(
                    dir,
                    lock,
                    options,
                    OpenReport::default(),
                    err,
                    maker,
                );
            }
        };
        let mut graph_file = read.file;
        guard_version(&dir, graph_file.version)?;
        if let Some(marker) = graph_file.backup {
            return Err(PersistError::IsBackup {
                path: dir.display(),
                commit_seq: marker.commit_seq,
            });
        }
        let mut report = OpenReport {
            clean_close: graph_file.clean,
            graph_file_repaired: read.repaired,
            warnings: backend.warnings,
            ..OpenReport::default()
        };

        if let Some(intent) = Intent::read(&dir)? {
            complete_intent(&dir, &intent)?;
            report.intent_completed = Some(intent.op);
            // An interrupted rollback or restore rewrote the lineage.
            graph_file = read_graph_file(&dir)?.file;
        }
        report.tmp_removed = remove_tmp(&dir)?;
        if let Some(cut) = graph_file.wal_cut.take() {
            report.refused_cut = apply_wal_cut(&dir, cut)?;
            // The segments after the cut held only refused bytes and are gone.
            if graph_file.wal_head.is_some_and(|head| head > cut.0) {
                graph_file.wal_head = Some(cut.0);
            }
        }

        let graph = maker.empty();
        let loaded = match load_latest(
            &dir,
            &options,
            &graph_file,
            &mut report,
            &mut *graph.write(),
        ) {
            Ok(loaded) => loaded,
            Err(err) if is_damage(&err) => {
                return Self::open_in_maintenance(dir, lock, options, report, err, maker);
            }
            Err(err) => return Err(err),
        };
        report.warnings.extend(manifest_copy_warnings(&dir));
        if let Some(reason) = loaded.read_only.clone() {
            return Self::open_read_only_mode(
                dir, lock, options, graph_file, report, graph, reason,
            );
        }
        let marks = sync_marks(&dir, &loaded, &options, &mut report)?;
        let segments = loaded.segments;
        let replay = loaded.replay;

        let chunk_bytes = graph_file.chunk_bytes;
        let graph_file = Arc::new(Mutex::new(graph_file));
        let wal = Arc::new(WalShared::default());
        wal.checkpoint_after
            .store(options.checkpoint_after_bytes, Ordering::Release);
        let journal = JournalCell::default();
        let durable = {
            let mut live = graph.write();
            // Refuses commits once damage found while the store is open froze it (the damage
            // policy `maintenance`, format spec 13.3); a user hook, so a reload keeps it.
            live.register_guard_hook(Box::new(DamageGuard {
                wal: wal.clone(),
                path: dir.display(),
            }))?;
            let durable = attach_journal(
                &dir,
                &mut *live,
                &journal,
                &options,
                &wal,
                &graph_file,
                &segments,
                &replay,
                &marks,
                DurableEnd::default(),
            )?;
            live.attach_journal(journal.slot())?;
            durable
        };
        {
            let mut file = graph_file.lock().map_err(|_| {
                PersistError::store(
                    "open the store",
                    StoreFailure::LockPoisoned,
                    "a store lock is poisoned",
                )
            })?;
            file.clean = false;
            write_graph_file(&dir, &file)?;
        }
        let workload_stats = workload::open_stats(&dir, options.workload_level, true);
        let shared = Arc::new(Shared {
            chunk_bytes,
            dir,
            graph,
            journal,
            options,
            graph_file,
            wal,
            durable,
            maintenance: Mutex::new(()),
            shutdown: AtomicBool::new(false),
            last_auto_error: Mutex::new(None),
            read_only: Mutex::new(None),
            stopped: Mutex::new(None),
            found: Mutex::new(Vec::new()),
            workload: workload_stats,
        });
        let thread = if shared.options.checkpoint_after_bytes > 0 {
            spawn_checkpoint_thread(&shared)
        } else {
            None
        };
        // A WAL already past the threshold at the open: the checkpoint is due now (otherwise
        // only the next commit would wake the thread).
        if thread.is_some()
            && shared.wal.since_checkpoint.load(Ordering::Acquire)
                >= shared.options.checkpoint_after_bytes
        {
            shared.wal.wake();
        }
        Ok(StoreCore {
            shared,
            report,
            maintenance: None,
            backup: None,
            lock: Mutex::new(Some(lock)),
            thread: Mutex::new(thread),
            closed: AtomicBool::new(false),
        })
    }

    fn open_read_only_mode(
        dir: DirPath,
        lock: DirLock,
        options: StoreOptions,
        graph_file: GraphFile,
        mut report: OpenReport,
        graph: Arc<dyn StoreGraph>,
        reason: String,
    ) -> Result<StoreCore, PersistError> {
        graph.write().register_guard_hook(Box::new(ReadOnlyHook {
            path: dir.display(),
            reason: reason.clone(),
        }))?;
        tracing::warn!("the store {} opens read-only: {reason}", dir.display());
        report.read_only = Some(reason.clone());
        let wal = Arc::new(WalShared::default());
        wal.closed.store(true, Ordering::Release);
        let workload_stats = workload::open_stats(&dir, options.workload_level, false);
        let shared = Arc::new(Shared {
            chunk_bytes: graph_file.chunk_bytes,
            dir,
            graph,
            journal: JournalCell::default(),
            options,
            graph_file: Arc::new(Mutex::new(graph_file)),
            wal,
            durable: DurableEnd::default(),
            maintenance: Mutex::new(()),
            shutdown: AtomicBool::new(false),
            last_auto_error: Mutex::new(None),
            read_only: Mutex::new(Some(reason)),
            stopped: Mutex::new(None),
            found: Mutex::new(Vec::new()),
            workload: workload_stats,
        });
        Ok(StoreCore {
            shared,
            report,
            maintenance: None,
            backup: None,
            lock: Mutex::new(Some(lock)),
            thread: Mutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    pub fn read_only_reason(&self) -> Result<Option<String>, PersistError> {
        Ok(poisoned_or(&self.shared.read_only, "read why the store is read-only")?.clone())
    }

    pub fn stopped_reason(&self) -> Result<Option<String>, PersistError> {
        Ok(poisoned_or(&self.shared.stopped, "read why the store stopped")?.clone())
    }

    /// Whether the store may write nothing to its files: read-only (format spec 16) or stopped;
    /// a poisoned lock of either reason writes nothing as well.
    fn writes_nothing(&self) -> bool {
        !matches!(self.read_only_reason(), Ok(None)) || !matches!(self.stopped_reason(), Ok(None))
    }

    fn open_in_maintenance(
        dir: DirPath,
        lock: DirLock,
        options: StoreOptions,
        mut report: OpenReport,
        trigger: PersistError,
        maker: &dyn GraphMaker,
    ) -> Result<StoreCore, PersistError> {
        let salvage = damage::salvage(&dir, &options.read)?;
        let mut damage = salvage.report;
        damage
            .notes
            .insert(0, format!("the open found damage: {trigger}"));
        if damage.damage.is_empty() {
            damage
                .damage
                .push(damage::Damage::load(trigger.to_string()));
        }
        let graph = maker.salvaged(salvage.graph)?;
        let refusal = PersistError::Maintenance {
            path: dir.display(),
            damage: damage.damage.len(),
        };
        graph
            .write()
            .register_guard_hook(Box::new(MaintenanceHook {
                message: refusal.to_string(),
                damage: damage.damage.len(),
                path: dir.display(),
            }))?;
        tracing::warn!("{refusal}");
        report.maintenance = true;
        report.commit_seq = damage.commit_seq;
        report.snapshot_commit_seq = damage.base_snapshot.unwrap_or(0);
        let wal = Arc::new(WalShared::default());
        wal.closed.store(true, Ordering::Release);
        let workload_stats = workload::open_stats(&dir, options.workload_level, false);
        let shared = Arc::new(Shared {
            chunk_bytes: salvage.graph_file.chunk_bytes,
            dir,
            graph,
            journal: JournalCell::default(),
            options,
            graph_file: Arc::new(Mutex::new(salvage.graph_file)),
            wal,
            durable: DurableEnd::default(),
            maintenance: Mutex::new(()),
            shutdown: AtomicBool::new(false),
            last_auto_error: Mutex::new(None),
            read_only: Mutex::new(None),
            stopped: Mutex::new(None),
            found: Mutex::new(Vec::new()),
            workload: workload_stats,
        });
        Ok(StoreCore {
            shared,
            report,
            maintenance: Some(damage),
            backup: None,
            lock: Mutex::new(Some(lock)),
            thread: Mutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    pub fn maintenance(&self) -> Option<&DamageReport> {
        self.maintenance.as_ref()
    }

    pub fn repair_to(&self, new_dir: impl IntoStoreDir) -> Result<RepairReport, PersistError> {
        Self::repair_at(&self.shared.dir, &DirPath::root(new_dir.into_store_dir()))
    }

    fn repair_at(dir: &DirPath, new_dir: &DirPath) -> Result<RepairReport, PersistError> {
        // Never a lossy repair of files a newer Graphersal wrote (format spec 15).
        files::refuse_newer_version(dir)?;
        // A store of a released older format is read as upgraded (format spec 15.1): the
        // repaired store is of the current format.
        let models_from = dir.clone();
        let older = upgrade::older_view(dir);
        let dir = older.as_ref().unwrap_or(dir);
        if dir.join(files::GRAPH_FILE).exists() || dir.join(files::GRAPH_COPY).exists() {
            match read_graph_file(dir) {
                Ok(file) => guard_version(dir, file.file.version)?,
                Err(err @ PersistError::OlderFormat { .. }) => return Err(err),
                Err(_) => {}
            }
        } else if !dir.join(SNAPSHOTS_DIR).is_dir() {
            return Err(PersistError::NotAStore {
                path: dir.display(),
                reason: "it has no GRAPH file and no snapshots".into(),
            });
        }
        let salvage = damage::salvage(dir, &ReadOptions::new().with_max_value_depth(4096))?;
        let options = StoreOptions::new()
            .with_chunk_bytes(salvage.graph_file.chunk_bytes)
            .with_damage_policy(salvage.graph_file.damage_policy);
        let commit_seq = salvage.graph.last_commit_seq();
        let parent_id = salvage.graph.graph_id();
        let graph = salvage.graph.with_graph_id(crate::value::uuid::new_v7());
        let graph_id = graph.graph_id();
        let mut lineage = vec![LineageEntry {
            graph_id,
            branched_at: commit_seq,
            time: now(),
        }];
        lineage.extend(salvage.graph_file.lineage.iter().copied());
        Self::create_lineage(
            new_dir,
            &graph,
            lineage,
            WriteOptions::new()
                .with_parent(parent_id, commit_seq)
                .with_name("repaired"),
            &options,
        )?;
        // The marks of a damaged store are a report only: a failure to list them is reported,
        // it does not fail the repair.
        let (marks_not_carried, marks_unreadable) = match marks_up_to(dir, commit_seq) {
            Ok(marks) => (marks, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        // The intact models are carried; a damaged one is reported (format spec 4.5).
        let (models, models_not_carried) = models::copy_all_lenient(&models_from, new_dir);
        let verified = maintenance::verify(new_dir, None)?;
        if !verified.is_ok() {
            return Err(PersistError::store(
                "repair the store",
                StoreFailure::CopyDoesNotVerify,
                format!(
                    "the repaired store in {} does not verify: {:?}",
                    new_dir.display(),
                    verified.problems
                ),
            ));
        }
        Ok(RepairReport {
            location: new_dir.display(),
            graph_id,
            commit_seq,
            damage: salvage.report,
            repaired: salvage.repaired,
            lost_commits: salvage.lost_commits,
            lost_ranges: salvage.lost_ranges,
            lost_elements: salvage.lost_elements,
            diverged: salvage.diverged,
            marks_not_carried,
            models,
            models_not_carried,
            marks_unreadable,
        })
    }

    pub fn read_only_at<S: PersistentStorage>(
        &self,
        target: RecoveryTarget,
        empty: S,
    ) -> Result<Recovered<S>, PersistError> {
        Self::load_plan_at(&self.shared.dir, target, &self.shared.options.read)?.run_in(empty)
    }

    fn load_at(
        dir: &DirPath,
        target: RecoveryTarget,
        read: &ReadOptions,
    ) -> Result<Recovered, PersistError> {
        Self::load_plan_at(dir, target, read)?.run()
    }

    /// What [`StoreCore::load_at`] loads: the nearest snapshot at or before `target` and the
    /// WAL after it (up to a recorded cut).
    fn load_plan_at(
        dir: &DirPath,
        target: RecoveryTarget,
        read: &ReadOptions,
    ) -> Result<LoadPlan, PersistError> {
        files::refuse_other_version(dir)?;
        let graph_file = read_graph_file(dir)?.file;
        guard_version(dir, graph_file.version)?;
        let not_reached = |reason: String| PersistError::TargetNotReached {
            target: target.to_string(),
            reason,
        };
        // A mark is resolved through the marks file (or a WAL scan) to its commit.
        let mut segments = list_wal(dir)?;
        drop_refused_segments(&mut segments, graph_file.wal_cut);
        let resolved = match &target {
            RecoveryTarget::Mark(name) => {
                let marks = match read_marks(dir)? {
                    Some(marks) => marks,
                    None => scan_marks(&segments, read.max_value_depth()),
                };
                let mark = marks
                    .iter()
                    .find(|m| m.name() == name)
                    .ok_or_else(|| not_reached("no such mark in the store".into()))?;
                if let Some((oldest, _)) = list_snapshots(dir)?.first()
                    && mark.commit_seq() < *oldest
                {
                    return Err(not_reached(format!(
                        "the mark is at commit {}, before the oldest snapshot (commit {oldest}): \
                         a prune removed the history it needs",
                        mark.commit_seq()
                    )));
                }
                RecoveryTarget::CommitSeq(mark.commit_seq())
            }
            other => other.clone(),
        };
        let mut chosen = None;
        for (seq, path) in list_snapshots(dir)? {
            let listing = read_listing(&path)?;
            let fits = match &resolved {
                RecoveryTarget::CommitSeq(target) => seq <= *target,
                RecoveryTarget::Time(time) => listing.last_commit_at <= *time,
                _ => true,
            };
            if fits {
                chosen = Some((seq, path));
            }
        }
        let Some((seq, path)) = chosen else {
            return Err(not_reached(
                "no snapshot at or before it (pruned, or before the store's start)".into(),
            ));
        };
        Ok(LoadPlan {
            snapshot: path,
            journals: wal_after(&segments, seq)
                .iter()
                .map(|(first, path)| (path.clone(), cut_limit(graph_file.wal_cut, *first)))
                .collect(),
            target: resolved,
            read: read.clone(),
            chain: graph_file.chain(),
        })
    }

    pub fn open_report(&self) -> &OpenReport {
        &self.report
    }

    pub fn dir(&self) -> &Arc<dyn StoreDir> {
        self.shared.dir.store_dir()
    }

    pub fn location(&self) -> String {
        self.shared.dir.display()
    }

    pub fn durable_end(&self) -> DurablePosition {
        self.shared.durable.position()
    }

    pub fn last_auto_checkpoint_error(&self) -> Result<Option<Arc<PersistError>>, PersistError> {
        Ok(poisoned_or(
            &self.shared.last_auto_error,
            "read the last automatic checkpoint error",
        )?
        .clone())
    }

    pub fn checkpoint(&self, name: Option<&str>) -> Result<SnapshotInfo, PersistError> {
        self.writable("write a checkpoint")?;
        if self.closed.load(Ordering::Acquire) {
            return Err(PersistError::store(
                "write a checkpoint",
                StoreFailure::Closed,
                "the store is closed",
            ));
        }
        self.shared
            .checkpoint(name, true)
            .map(|report| report.snapshot)
            .inspect_err(|err| self.shared.report_checkpoint_damage(err))
    }

    pub fn mark(&self, name: &str) -> Result<Mark, PersistError> {
        self.writable("set a mark")?;
        // A listing hides a mark before the oldest snapshot: its name is still taken, say why.
        if let Some(old) = all_marks(&self.shared.dir)?
            .iter()
            .find(|mark| mark.name() == name)
            && !list_marks(&self.shared.dir)?
                .iter()
                .any(|mark| mark.name() == name)
        {
            return Err(PersistError::store(
                "set a mark",
                StoreFailure::NameTaken,
                format!(
                    "the name {name:?} belongs to the mark at commit {}, whose history a prune \
                     removed; mark names stay unique along the store's whole history",
                    old.commit_seq()
                ),
            ));
        }
        Ok(self.shared.graph.write().mark(name)?)
    }

    pub fn snapshots(&self) -> Result<Vec<SnapshotInfo>, PersistError> {
        list_snapshot_infos(&self.shared.dir)
    }

    pub fn marks(&self) -> Result<Vec<Mark>, PersistError> {
        list_marks(&self.shared.dir)
    }

    pub fn info(&self) -> Result<StoreInfo, PersistError> {
        let (commit_seq, last_commit_at) = {
            let graph = self.shared.graph.read();
            (graph.last_commit_seq()?, graph.last_committed_at())
        };
        let file = self
            .shared
            .graph_file
            .lock()
            .map_err(|_| {
                PersistError::store(
                    "read the store info",
                    StoreFailure::LockPoisoned,
                    "a store lock is poisoned",
                )
            })?
            .clone();
        let mut info = info_of(&self.shared.dir, &file)?;
        info.commit_seq = commit_seq;
        info.last_commit_at = last_commit_at;
        info.durable = self.durable_position();
        info.clean = self.backup.is_some();
        info.definitions_by_kind =
            count_by_kind(self.shared.graph.read().definition_kinds()?.into_iter());
        info.definitions = info.definitions_by_kind.values().sum();
        info.schema_models = self.shared.graph.read().schema_models()?;
        info.damage_found = self.damage_found()?;
        info.frozen_by_damage = self.is_frozen_by_damage();
        info.stopped = self.stopped_reason()?;
        Ok(info)
    }

    pub fn export_packed(
        &self,
        commit_seq: Option<u64>,
        writer: impl std::io::Write,
        models: ModelCopy,
    ) -> Result<SnapshotInfo, PersistError> {
        export_packed_from(&self.shared.dir, commit_seq, writer, models)
    }

    pub fn fork(
        &self,
        target: RecoveryTarget,
        new_dir: impl IntoStoreDir,
    ) -> Result<ForkReport, PersistError> {
        Self::fork_at(
            &self.shared.dir,
            target,
            &DirPath::root(new_dir.into_store_dir()),
        )
    }

    fn fork_at(
        dir: &DirPath,
        target: RecoveryTarget,
        new_dir: &DirPath,
    ) -> Result<ForkReport, PersistError> {
        files::refuse_other_version(dir)?;
        let parent = read_graph_file(dir)?.file;
        let options = StoreOptions::new()
            .with_chunk_bytes(parent.chunk_bytes)
            .with_damage_policy(parent.damage_policy);
        let recovered = Self::load_at(dir, target, &options.read)?;
        let branched_at = recovered.graph.last_commit_seq();
        // Listed before anything is written: a fork that cannot report them writes nothing.
        let marks_not_carried = marks_up_to(dir, branched_at)?;
        let parent_id = recovered.graph.graph_id();
        let graph = recovered.graph.with_graph_id(crate::value::uuid::new_v7());
        let mut lineage = vec![LineageEntry {
            graph_id: graph.graph_id(),
            branched_at,
            time: now(),
        }];
        lineage.extend(parent.lineage.iter().copied());
        let existed = new_dir.exists();
        Self::create_lineage(
            new_dir,
            &graph,
            lineage,
            WriteOptions::new().with_parent(parent_id, branched_at),
            &options,
        )?;
        let models = models::carry_into_new(dir, new_dir, existed)?;
        let (_, path) = list_snapshots(new_dir)?.pop().ok_or_else(|| {
            PersistError::store(
                "fork the store",
                StoreFailure::ChangedConcurrently,
                "the new store has no snapshot",
            )
        })?;
        Ok(ForkReport {
            snapshot: read_listing(&path)?,
            marks_not_carried,
            models,
        })
    }

    pub fn prune(&self, up_to: u64) -> Result<PruneReport, PersistError> {
        self.writable("prune the store")?;
        let _guard = self.shared.maintenance.lock().map_err(|_| {
            PersistError::store(
                "prune the store",
                StoreFailure::LockPoisoned,
                "a store lock is poisoned",
            )
        })?;
        let report = maintenance::prune(&self.shared.dir, up_to)?;
        if self.shared.options.auto_compact
            && compaction::advise(&self.shared.dir, self.shared.options.compaction, None)?
                .recommended
        {
            self.shared
                .dir
                .store_dir()
                .compact()
                .map_err(|err| PersistError::io("compacting the store", err))?;
        }
        Ok(report)
    }

    pub fn compact(&self) -> Result<CompactReport, PersistError> {
        self.writable("compact the store")?;
        let up_to = self.shared.graph.read().last_commit_seq()?;
        let _guard = self.shared.maintenance.lock().map_err(|_| {
            PersistError::store(
                "compact the store",
                StoreFailure::LockPoisoned,
                "a store lock is poisoned",
            )
        })?;
        let prune = maintenance::prune(&self.shared.dir, up_to)?;
        let compaction = self
            .shared
            .dir
            .store_dir()
            .compact()
            .map_err(|err| PersistError::io("compacting the store", err))?;
        Ok(CompactReport { prune, compaction })
    }

    pub fn compaction_advice(&self) -> Result<CompactionAdvice, PersistError> {
        self.compaction_advice_with(self.shared.options.compaction)
    }

    pub fn compaction_advice_with(
        &self,
        policy: CompactionPolicy,
    ) -> Result<CompactionAdvice, PersistError> {
        let blocked = self
            .writable("compact the store")
            .err()
            .map(|e| e.to_string());
        compaction::advise(&self.shared.dir, policy, blocked)
    }

    pub fn verify(&self) -> Result<VerifyReport, PersistError> {
        let report = match maintenance::verify(&self.shared.dir, self.durable_position()) {
            Ok(report) => report,
            // Both GRAPH copies unusable: damage of the open store's files too.
            Err(err @ PersistError::NotAStore { .. }) => {
                if self.tracks_damage() {
                    self.shared
                        .report_damage("verify", vec![problem_of(&self.shared.dir, &err)]);
                }
                return Err(err);
            }
            Err(err) => return Err(err),
        };
        if !report.is_ok() && self.tracks_damage() {
            self.shared.report_damage("verify", report.problems.clone());
        }
        Ok(report)
    }

    fn tracks_damage(&self) -> bool {
        self.maintenance.is_none() && self.backup.is_none()
    }

    pub fn damage_policy(&self) -> Result<DamagePolicy, PersistError> {
        Ok(poisoned_or(&self.shared.graph_file, "read the damage policy")?.damage_policy)
    }

    pub fn store_id(&self) -> Result<u128, PersistError> {
        Ok(poisoned_or(&self.shared.graph_file, "read the store id")?.store_id)
    }

    pub fn damage_found(&self) -> Result<Vec<FoundDamage>, PersistError> {
        Ok(poisoned_or(&self.shared.found, "read the damage found")?.clone())
    }

    pub fn is_frozen_by_damage(&self) -> bool {
        self.shared.wal.frozen().is_some()
    }

    fn durable_position(&self) -> Option<DurablePosition> {
        self.backup
            .is_none()
            .then(|| self.shared.durable.position())
    }

    pub fn backup(&self, target_dir: impl IntoStoreDir) -> Result<BackupReport, PersistError> {
        self.backup_with(target_dir, BackupMode::Auto)
    }

    pub fn backup_with(
        &self,
        target_dir: impl IntoStoreDir,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError> {
        let result = backup::backup_into(
            &self.shared.dir,
            self.durable_position(),
            &DirPath::root(target_dir.into_store_dir()),
            mode,
        );
        self.after_backup(result)
    }

    pub fn backup_from_memory(
        &self,
        target_dir: impl IntoStoreDir,
        mode: BackupMode,
    ) -> Result<BackupReport, PersistError> {
        let refused = |reason: String| PersistError::BackupRefused { reason };
        if let Some(marker) = &self.backup {
            return Err(PersistError::IsBackup {
                path: self.shared.dir.display(),
                commit_seq: marker.commit_seq,
            });
        }
        if self.maintenance.is_some() {
            return Err(refused(format!(
                "the store {} opened in maintenance mode: its graph in memory is the state \
                 salvaged from donors, not a verified one. Build a repaired copy instead: \
                 graphersal store repair {} --to <new_dir> (Store::repair_to)",
                self.shared.dir, self.shared.dir
            )));
        }
        if self.damage_found()?.is_empty() {
            return Err(refused(format!(
                "a backup from memory is only for a store whose files are damaged, and no \
                 damage was found in {} while it is open: back up normally (Store::backup; it \
                 reads every checksum and, when it finds damage, says so; then a backup from \
                 memory is possible)",
                self.shared.dir
            )));
        }
        let graph_file = self
            .shared
            .graph_file
            .lock()
            .map_err(|_| {
                PersistError::store(
                    "back up the store",
                    StoreFailure::LockPoisoned,
                    "a store lock is poisoned",
                )
            })?
            .clone();
        let write = checkpoint_write_options(
            &self.shared.options,
            self.shared.chunk_bytes,
            from_memory::FROM_MEMORY_NAME,
        );
        let target = DirPath::root(target_dir.into_store_dir());
        let graph = self.shared.graph.read();
        graph.backup_from_memory(&self.shared.dir, &graph_file, &write, &target, mode)
    }

    fn after_backup(
        &self,
        result: Result<BackupReport, PersistError>,
    ) -> Result<BackupReport, PersistError> {
        match result {
            Err(PersistError::BackupDamaged(mut damage))
                if damage.side == super::DamageSide::Store && self.tracks_damage() =>
            {
                self.shared.report_damage("backup", damage.problems.clone());
                damage.from_memory = true;
                Err(PersistError::BackupDamaged(damage))
            }
            other => other,
        }
    }

    #[cfg(feature = "persist-zip")]
    pub fn backup_zip<W>(
        &self,
        mut archive: W,
        models: ModelCopy,
    ) -> Result<BackupReport, PersistError>
    where
        W: std::io::Read + std::io::Write + std::io::Seek + Send,
    {
        let result = maintenance::backup_to_zip(
            &self.shared.dir,
            self.durable_position(),
            &mut archive,
            models,
        );
        self.after_backup(result)
    }

    pub fn backup_marker(&self) -> Option<BackupMarker> {
        self.backup
    }

    pub fn changes_since(&self, commit_seq: u64) -> Result<ChangesSince, PersistError> {
        maintenance::changes_since(
            &self.shared.dir,
            commit_seq,
            self.durable_position(),
            self.shared.options.read.max_value_depth(),
        )
    }

    pub fn rollback_to(
        &self,
        target: RecoveryTarget,
        mode: RollbackMode,
    ) -> Result<RollbackReport, PersistError> {
        self.writable("roll the store back")?;
        let _guard = self.shared.maintenance.lock().map_err(|_| {
            PersistError::store(
                "roll the store back",
                StoreFailure::LockPoisoned,
                "a store lock is poisoned",
            )
        })?;
        let mut graph = self.shared.graph.write();
        if let Some(Err(err)) = self.shared.journal.sync_now() {
            return Err(PersistError::store(
                "roll the store back",
                StoreFailure::JournalSyncFailed,
                err.to_string(),
            ));
        }
        let current = graph.last_commit_seq()?;
        let dir = &self.shared.dir;
        let reason = format!("rollback to {target}");
        let target = match target {
            RecoveryTarget::CommitSeq(seq) => seq,
            RecoveryTarget::Latest => current,
            other => Self::load_at(dir, other, &self.shared.options.read)?
                .graph
                .last_commit_seq(),
        };
        if target >= current {
            return Err(PersistError::store(
                "roll the store back",
                StoreFailure::NothingAfterTarget,
                format!("the store is at commit {current}: there is nothing after commit {target}"),
            ));
        }
        let intent = rollback::plan_rollback(dir, target, current, mode, &reason)?;
        let attic = (mode == RollbackMode::Move)
            .then(|| {
                intent
                    .param("attic")
                    .map(|a| a.trim_start_matches("attic/").to_string())
            })
            .flatten();
        // (Directories without a manifest, holders of referenced files, move too.)
        let snapshots_moved: Vec<u64> = files::list_snapshots(dir)?
            .into_iter()
            .map(|(seq, _)| seq)
            .filter(|seq| *seq > target)
            .collect();
        let segments_moved = intent
            .files
            .iter()
            .filter(|f| f.starts_with("wal/"))
            .count()
            + usize::from(intent.param("split").is_some());
        intent.write(dir)?;
        rollback::apply_rollback(dir, &intent, None)
            .map_err(|err| self.stop_after_failed_change("the in-place rollback", err))?;
        let graph_id = self
            .reload_in_place(&mut *graph)
            .map_err(|err| self.stop_after_failed_reload("roll the store back", "rollback", err))?;
        Ok(RollbackReport {
            target,
            previous_commit_seq: current,
            graph_id,
            snapshots_moved,
            segments_moved,
            attic,
        })
    }

    pub fn attic_list(&self) -> Result<Vec<AtticEntry>, PersistError> {
        rollback::attic_list(&self.shared.dir)
    }

    pub fn attic_changes(&self, id: &str) -> Result<ChangesSince, PersistError> {
        Self::attic_changes_at(&self.shared.dir, id)
    }

    fn attic_changes_at(dir: &DirPath, id: &str) -> Result<ChangesSince, PersistError> {
        let (entry, segments) = rollback::attic_segments(dir, id)?;
        Ok(maintenance::changes_over(
            segments,
            entry.target,
            crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
        ))
    }

    pub fn attic_restore(&self, id: &str) -> Result<u64, PersistError> {
        self.writable("restore the attic entry")?;
        let _guard = self.shared.maintenance.lock().map_err(|_| {
            PersistError::store(
                "restore the attic entry",
                StoreFailure::LockPoisoned,
                "a store lock is poisoned",
            )
        })?;
        let mut graph = self.shared.graph.write();
        if let Some(Err(err)) = self.shared.journal.sync_now() {
            return Err(PersistError::store(
                "restore the attic entry",
                StoreFailure::JournalSyncFailed,
                err.to_string(),
            ));
        }
        let dir = &self.shared.dir;
        let intent = rollback::plan_restore(dir, id, graph.last_commit_seq()?)?;
        intent.write(dir)?;
        rollback::apply_restore(dir, &intent, None)
            .map_err(|err| self.stop_after_failed_change("the attic restore", err))?;
        self.reload_in_place(&mut *graph).map_err(|err| {
            self.stop_after_failed_reload("restore the attic entry", "restore", err)
        })?;
        Ok(graph.last_commit_seq()?)
    }

    pub fn attic_can_restore(&self, id: &str) -> Result<(), PersistError> {
        self.writable("restore the attic entry")?;
        let commit_seq = self.shared.graph.read().last_commit_seq()?;
        rollback::plan_restore(&self.shared.dir, id, commit_seq).map(|_| ())
    }

    pub fn attic_fork(
        &self,
        id: &str,
        new_dir: impl IntoStoreDir,
    ) -> Result<ForkReport, PersistError> {
        rollback::attic_fork(
            &self.shared.dir,
            id,
            &DirPath::root(new_dir.into_store_dir()),
        )
    }

    pub fn attic_remove(&self, id: &str) -> Result<(), PersistError> {
        self.writable("remove the attic entry")?;
        let _guard = self.shared.maintenance.lock().map_err(|_| {
            PersistError::store(
                "remove the attic entry",
                StoreFailure::LockPoisoned,
                "a store lock is poisoned",
            )
        })?;
        rollback::attic_remove(&self.shared.dir, id)
    }

    /// Stops the store after an in-place change failed while its files were changed (the
    /// graph in memory still holds the state before it): nothing more is written, the next
    /// open completes or undoes the change from the `INTENT`.
    fn stop_after_failed_change(&self, operation: &str, err: PersistError) -> PersistError {
        self.stop(format!(
            "{operation} failed ({err}); close and reopen the store to complete or undo it"
        ));
        err
    }

    /// Stops the store after the reload of an in-place change failed. The graph was emptied
    /// already ([`Self::reload_in_place`]); the error says so ([`StoreFailure::ReloadFailed`]).
    fn stop_after_failed_reload(
        &self,
        operation: &'static str,
        change: &str,
        err: PersistError,
    ) -> PersistError {
        self.stop(format!(
            "the reload after the {change} failed ({err}): the graph in memory was emptied; \
             close and reopen the store to complete the {change}"
        ));
        PersistError::store(
            operation,
            StoreFailure::ReloadFailed,
            format!(
                "the reload of the graph after the {change} failed ({err}); the graph in \
                 memory was emptied, the store is stopped and writes nothing more; the files \
                 on disk are intact and the {change} completes (or is undone) at the next open: \
                 close and reopen the store"
            ),
        )
    }

    fn stop(&self, reason: String) {
        self.shared.wal.closed.store(true, Ordering::Release);
        if let Ok(mut stopped) = self.shared.stopped.lock() {
            *stopped = Some(reason);
        }
    }

    fn writable(&self, operation: &'static str) -> Result<(), PersistError> {
        if let Some(marker) = &self.backup {
            return Err(PersistError::IsBackup {
                path: self.shared.dir.display(),
                commit_seq: marker.commit_seq,
            });
        }
        if let Some(report) = &self.maintenance {
            return Err(PersistError::Maintenance {
                path: self.shared.dir.display(),
                damage: report.damage.len(),
            });
        }
        if let Some(reason) = self.read_only_reason()? {
            return Err(PersistError::ReadOnly {
                path: self.shared.dir.display(),
                reason,
            });
        }
        if let Some(reason) = self.stopped_reason()? {
            return Err(PersistError::store(
                operation,
                StoreFailure::Stopped,
                reason,
            ));
        }
        if let Some((found_by, problems)) = self.shared.wal.frozen() {
            return Err(PersistError::DamageFound {
                path: self.shared.dir.display(),
                found_by,
                problems,
            });
        }
        if self.closed.load(Ordering::Acquire) || self.shared.wal.closed.load(Ordering::Acquire) {
            return Err(PersistError::store(
                operation,
                StoreFailure::Closed,
                "the store is closed",
            ));
        }
        Ok(())
    }

    /// Loads the store's current state into the live graph after an in-place change. On any
    /// failure the graph is emptied again (an empty graph, never a partial one) before the
    /// error is returned; the caller stops the store.
    fn reload_in_place(&self, graph: &mut dyn LiveWrite) -> Result<u128, PersistError> {
        self.reload_in_place_inner(graph).inspect_err(|_| {
            // The reload's error is the one reported (the caller stops the store); a storage
            // that cannot even empty itself is logged beside it.
            if let Err(err) = graph.clear() {
                tracing::error!("emptying the graph after a failed reload failed too: {err}");
            }
        })
    }

    fn reload_in_place_inner(&self, graph: &mut dyn LiveWrite) -> Result<u128, PersistError> {
        let dir = &self.shared.dir;
        // The old journal goes first: no record may reach its segment any more (commits and
        // marks are refused until the new one is in, also when the reload fails).
        self.shared.journal.set(None);
        let graph_file = read_graph_file(dir)?.file;
        let mut report = OpenReport::default();
        // Cleared and loaded in place: the hooks registered on the graph stay.
        let loaded = load_latest(dir, &self.shared.options, &graph_file, &mut report, graph)?;
        let graph_id = graph_file.graph_id;
        if let Some(reason) = loaded.read_only.clone() {
            // The restored history holds something critical this version does not know: the
            // store goes read-only (no journal), as an open of it would.
            graph.register_guard_hook(Box::new(ReadOnlyHook {
                path: dir.display(),
                reason: reason.clone(),
            }))?;
            if let Ok(mut file) = self.shared.graph_file.lock() {
                *file = graph_file;
            }
            self.shared.wal.closed.store(true, Ordering::Release);
            if let Ok(mut read_only) = self.shared.read_only.lock() {
                *read_only = Some(reason);
            }
            return Ok(graph_id);
        }
        let marks = sync_marks(dir, &loaded, &self.shared.options, &mut report)?;
        if let Ok(mut file) = self.shared.graph_file.lock() {
            let clean = file.clean;
            *file = graph_file;
            file.clean = clean;
        }
        attach_journal(
            dir,
            graph,
            &self.shared.journal,
            &self.shared.options,
            &self.shared.wal,
            &self.shared.graph_file,
            &loaded.segments,
            &loaded.replay,
            &marks,
            self.shared.durable.clone(),
        )?;
        self.shared.wal.rotate.store(false, Ordering::Release);
        Ok(graph_id)
    }

    pub fn shutdown(&self) -> Result<(), PersistError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.stop_thread();
        // The workload statistics first (best effort: a failure is logged, the close goes on).
        let writable = self.backup.is_none()
            && self.maintenance.is_none()
            && self
                .shared
                .read_only
                .lock()
                .is_ok_and(|reason| reason.is_none())
            && self
                .shared
                .stopped
                .lock()
                .is_ok_and(|reason| reason.is_none())
            && self.shared.wal.frozen().is_none();
        if let Err(err) = self.shared.flush_workload(writable) {
            tracing::warn!(%err, "the workload statistics were not written at close");
        }
        let mut result = Ok(());
        if self.shared.options.checkpoint_on_close
            && !self.writes_nothing()
            && self.shared.wal.frozen().is_none()
            && let Err(err) = self.shared.checkpoint(None, true)
        {
            result = Err(err);
        }
        let _guard = self.shared.maintenance.lock();
        {
            let _graph = self.shared.graph.write();
            if let Some(Err(err)) = self.shared.journal.sync_now()
                && result.is_ok()
            {
                result = Err(PersistError::store(
                    "close the store",
                    StoreFailure::JournalSyncFailed,
                    err.to_string(),
                ));
            }
            // From now on every commit is refused (vetoed and rolled back).
            self.shared.wal.closed.store(true, Ordering::Release);
        }
        let last_commit = self.shared.graph.read().last_commit_seq()?;
        // A store frozen by damage found while it was open writes nothing more to its disk
        // (it opens as "not closed cleanly" next time, which is no damage).
        if result.is_ok()
            && self.maintenance.is_none()
            && self.backup.is_none()
            && !self.writes_nothing()
            && self.shared.wal.frozen().is_none()
            && let Ok(mut file) = self.shared.graph_file.lock()
        {
            file.clean = true;
            file.closed_at = Some(last_commit);
            result = write_graph_file(&self.shared.dir, &file);
        }
        if let Ok(mut lock) = self.lock.lock() {
            *lock = None;
        }
        result
    }

    pub fn run_due_checkpoint(&self) -> Result<Option<SnapshotInfo>, PersistError> {
        let has_thread = self.thread.lock().map(|t| t.is_some()).unwrap_or(false);
        let limit = self.shared.wal.checkpoint_after.load(Ordering::Acquire);
        if has_thread
            || limit == 0
            || self.maintenance.is_some()
            || self.writes_nothing()
            || self.shared.wal.frozen().is_some()
            || self.closed.load(Ordering::Acquire)
            || self.shared.wal.since_checkpoint.load(Ordering::Acquire) < limit
        {
            return Ok(None);
        }
        // Only the checkpoint's own record since the last snapshot: nothing to write.
        let current = self.shared.graph.read().last_commit_seq()?;
        if list_snapshot_infos(&self.shared.dir)?
            .last()
            .is_some_and(|s| s.commit_seq == current)
        {
            return Ok(None);
        }
        let result = self
            .shared
            .checkpoint(None, false)
            .map(|report| report.snapshot)
            .inspect_err(|err| self.shared.report_checkpoint_damage(err));
        let failed = match &result {
            Ok(_) => None,
            Err(err) => Some(Arc::new(err.duplicate())),
        };
        *self
            .shared
            .last_auto_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = failed;
        result.map(Some)
    }

    pub fn wal_bytes_since_checkpoint(&self) -> u64 {
        self.shared.wal.since_checkpoint.load(Ordering::Acquire)
    }

    fn stop_thread(&self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wal.wake();
        let thread = self.thread.lock().ok().and_then(|mut t| t.take());
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl Drop for StoreCore {
    fn drop(&mut self) {
        if let Err(err) = self.shutdown() {
            tracing::warn!("closing the store {} failed: {err}", self.shared.dir);
        }
    }
}

/// The snapshot at `seq` among `existing`, when there is one: a checkpoint there returns it
/// (a different `name` is an error).
fn existing_snapshot(
    existing: &[SnapshotInfo],
    seq: u64,
    name: &str,
) -> Result<Option<SnapshotInfo>, PersistError> {
    let Some(same) = existing.iter().find(|s| s.commit_seq == seq) else {
        return Ok(None);
    };
    if !name.is_empty() && same.name != name {
        return Err(PersistError::store(
            "write a checkpoint",
            StoreFailure::SnapshotExists,
            format!(
                "a snapshot at commit {seq} already exists{}; commit something first",
                if same.name.is_empty() {
                    String::new()
                } else {
                    format!(" (named {:?})", same.name)
                }
            ),
        ));
    }
    Ok(Some(same.clone()))
}

/// Refuses a name longer than 255 bytes or used by another snapshot.
fn check_snapshot_name(existing: &[SnapshotInfo], name: &str) -> Result<(), PersistError> {
    if name.len() > 255 {
        return Err(PersistError::store(
            "write a checkpoint",
            StoreFailure::NameTooLong,
            "a snapshot name has at most 255 bytes",
        ));
    }
    if !name.is_empty() && existing.iter().any(|s| s.name == name) {
        return Err(PersistError::store(
            "write a checkpoint",
            StoreFailure::NameTaken,
            format!("a snapshot named {name:?} already exists"),
        ));
    }
    Ok(())
}

/// The write options of a store's checkpoint.
fn checkpoint_write_options(options: &StoreOptions, chunk_bytes: u32, name: &str) -> WriteOptions {
    WriteOptions::new()
        .with_name(name)
        .with_codec(options.codec)
        .with_chunk_bytes(chunk_bytes)
        .with_segment_bytes(options.snapshot_segment_bytes)
}

/// What a checkpoint did ([`Store::checkpoint_dir`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckpointReport {
    /// The snapshot: the new one, or the existing one at the store's position when nothing was
    /// committed after it.
    pub snapshot: SnapshotInfo,
    /// Whether a new snapshot was written.
    pub written: bool,
    /// Whether it was merged from the previous snapshot and the WAL after it (format spec
    /// 12.3); `false`: encoded from the loaded graph (the WAL since the last snapshot exceeded
    /// [`StoreOptions::with_checkpoint_merge_bytes`]), or not written.
    pub merged: bool,
    /// The snapshot it was merged from (`0` when not merged).
    pub base_commit_seq: u64,
    /// The commits merged into it.
    pub commits_merged: u64,
    /// The segment files of unchanged id ranges the new snapshot references (twins held by
    /// the snapshot before the previous one, format spec 12.3: the new snapshot never shares
    /// a file with the previous one, which stays the donor of every id range).
    pub segments_reused: usize,
    /// The segment files of unchanged id ranges copied byte for byte into the new snapshot
    /// (no twin existed: the first checkpoint after the range was written).
    pub segments_copied: usize,
    /// The bytes of [`Self::segments_copied`].
    pub bytes_copied: u64,
    /// The segment files written from merged data (or encoded from the graph).
    pub segments_written: usize,
}

impl CheckpointReport {
    fn existing(snapshot: SnapshotInfo) -> Self {
        Self {
            snapshot,
            written: false,
            merged: false,
            base_commit_seq: 0,
            commits_merged: 0,
            segments_reused: 0,
            segments_copied: 0,
            bytes_copied: 0,
            segments_written: 0,
        }
    }

    fn encoded(snapshot: SnapshotInfo) -> Self {
        Self {
            segments_written: snapshot.segment_count as usize,
            written: true,
            ..Self::existing(snapshot)
        }
    }

    fn merged(snapshot: SnapshotInfo, stats: &merge::MergeStats) -> Self {
        Self {
            snapshot,
            written: true,
            merged: true,
            base_commit_seq: stats.base_commit_seq,
            commits_merged: stats.commits,
            segments_reused: stats.segments_reused,
            segments_copied: stats.segments_copied,
            bytes_copied: stats.bytes_copied,
            segments_written: stats.segments_written,
        }
    }
}

impl Shared {
    /// Records damage found on disk while the store is open (format spec 13.3): logged, kept
    /// for [`Store::damage_found`], and, with the damage policy `maintenance`, the store turns
    /// read-only (every later commit, mark and checkpoint is refused). Problems of attic
    /// entries only (history moved aside, not the store's own) are reported by `verify` but do
    /// not count here.
    fn report_damage(&self, found_by: &str, problems: Vec<VerifyProblem>) {
        let problems: Vec<VerifyProblem> = problems
            .into_iter()
            // Neither the attic nor a model (format spec 4.5) is graph data the store writes.
            .filter(|p| p.path != "attic" && !p.path.starts_with("attic/"))
            .filter(|p| p.path != models::MODELS_DIR && !p.path.starts_with("models/"))
            .collect();
        if problems.is_empty() {
            return;
        }
        // A poisoned lock still holds the file as last written: the policy is fixed at
        // creation, so reading it through the poison is exact.
        let policy = self
            .graph_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .damage_policy;
        tracing::warn!(
            "{found_by} found damage in the store {} ({} problem(s), first: {}: {}); damage \
             policy {policy}: {}",
            self.dir,
            problems.len(),
            problems[0].path,
            problems[0].reason,
            match policy {
                DamagePolicy::Maintenance => "the store is read-only from now on",
                DamagePolicy::Continue => "the store keeps committing",
            }
        );
        let count = problems.len();
        if let Ok(mut found) = self.found.lock() {
            found.push(FoundDamage {
                found_by: found_by.to_string(),
                at: now(),
                problems,
            });
        }
        if policy == DamagePolicy::Maintenance
            && let Ok(mut frozen) = self.wal.frozen.lock()
            && frozen.is_none()
        {
            *frozen = Some((found_by.to_string(), count));
        }
    }

    /// Reports the damage a failed checkpoint ran into (a merge reads the newest snapshot and
    /// the WAL after it).
    fn report_checkpoint_damage(&self, err: &PersistError) {
        if is_damage(err) {
            self.report_damage("checkpoint", vec![problem_of(&self.dir, err)]);
        }
    }

    /// A checkpoint of the open store (format spec 12.3). The new snapshot is merged from the
    /// newest one and the WAL up to the durable end ([`merge`]), without the graph's lock: the
    /// graph is locked only to sync the journal first (`sync`, when the durable end lags the
    /// last commit) and to append the Checkpoint record afterwards. Beyond the merge bound the
    /// loaded graph is encoded under its read lock.
    fn checkpoint(&self, name: Option<&str>, sync: bool) -> Result<CheckpointReport, PersistError> {
        let _guard = self.maintenance.lock().map_err(|_| {
            PersistError::store(
                "write a checkpoint",
                StoreFailure::LockPoisoned,
                "a store lock is poisoned",
            )
        })?;
        if self.wal.closed.load(Ordering::Acquire) {
            return Err(PersistError::store(
                "write a checkpoint",
                StoreFailure::Closed,
                "the store is closed",
            ));
        }
        let name = name.unwrap_or_default();
        if sync {
            let graph = self.graph.write();
            let last = graph.last_commit_seq()?;
            if self.durable.commit_seq() < last
                && let Some(Err(err)) = self.journal.sync_now()
            {
                return Err(PersistError::store(
                    "write a checkpoint",
                    StoreFailure::JournalSyncFailed,
                    err.to_string(),
                ));
            }
        }
        let written_since = self.wal.since_checkpoint.load(Ordering::Acquire);
        let durable = self.durable.position();
        let existing = list_snapshot_infos(&self.dir)?;
        if let Some(same) = existing_snapshot(&existing, durable.commit_seq, name)? {
            return Ok(CheckpointReport::existing(same));
        }
        check_snapshot_name(&existing, name)?;
        let write = checkpoint_write_options(&self.options, self.chunk_bytes, name);
        let merged = if self.options.checkpoint_merge_bytes == 0 {
            merge::Merged::TooLarge { wal_bytes: 0 }
        } else {
            let graph_file = self
                .graph_file
                .lock()
                .map_err(|_| {
                    PersistError::store(
                        "write a checkpoint",
                        StoreFailure::LockPoisoned,
                        "a store lock is poisoned",
                    )
                })?
                .clone();
            merge::merge_checkpoint(&merge::MergeRequest {
                dir: &self.dir,
                graph_file: &graph_file,
                durable: Some(durable),
                write: &write,
                read: &self.options.read,
                max_wal_bytes: self.options.checkpoint_merge_bytes,
            })?
        };
        let report = match merged {
            merge::Merged::Written { info, stats } => CheckpointReport::merged(*info, &stats),
            merge::Merged::Current { commit_seq } => {
                let existing = list_snapshot_infos(&self.dir)?;
                return match existing_snapshot(&existing, commit_seq, name)? {
                    Some(same) => Ok(CheckpointReport::existing(same)),
                    None => Err(PersistError::store(
                        "write a checkpoint",
                        StoreFailure::ChangedConcurrently,
                        format!("the snapshot at commit {commit_seq} vanished"),
                    )),
                };
            }
            merge::Merged::TooLarge { wal_bytes } => {
                if wal_bytes > 0 {
                    tracing::info!(
                        "checkpoint of {}: {wal_bytes} WAL bytes since the last snapshot exceed \
                         the merge bound; the loaded graph is encoded instead",
                        self.dir
                    );
                }
                self.checkpoint_from_graph(&write, name)?
            }
            merge::Merged::NoBase => self.checkpoint_from_graph(&write, name)?,
        };
        if !report.written {
            return Ok(report);
        }
        self.wal.rotate.store(true, Ordering::Release);
        // A compare-exchange loop rather than `fetch_update` (deprecated in Rust 1.99 for
        // `try_update`, which is newer than the MSRV).
        let since = &self.wal.since_checkpoint;
        let mut current = since.load(Ordering::Acquire);
        while let Err(actual) = since.compare_exchange_weak(
            current,
            current.saturating_sub(written_since),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            current = actual;
        }
        let _graph = self.graph.write();
        if let Some(Err(err)) = self
            .journal
            .record_checkpoint(report.snapshot.commit_seq, now())
        {
            return Err(PersistError::store(
                "record the checkpoint in the WAL",
                StoreFailure::JournalSyncFailed,
                err.to_string(),
            ));
        }
        Ok(report)
    }

    /// A snapshot encoded from the loaded graph (the streaming encoder of format spec 8): the
    /// read lock is held while the encoder reads the graph and writes the files; syncing the
    /// directory, verifying and renaming run without it.
    fn checkpoint_from_graph(
        &self,
        write: &WriteOptions,
        name: &str,
    ) -> Result<CheckpointReport, PersistError> {
        let snapshot = {
            let graph = self.graph.read();
            let seq = graph.last_commit_seq()?;
            let existing = list_snapshot_infos(&self.dir)?;
            if let Some(same) = existing_snapshot(&existing, seq, name)? {
                return Ok(CheckpointReport::existing(same));
            }
            let snapshot = NewSnapshotDir::begin(&self.dir, seq)?;
            graph.write_snapshot_live(snapshot.tmp(), write)?;
            snapshot
        };
        let (_, mut info) = snapshot.finish()?;
        info.meta.clear();
        Ok(CheckpointReport::encoded(info))
    }
}

/// Starts the checkpoint thread (`None` where the platform has no threads: wasm32, where
/// [`Store::run_due_checkpoint`] runs it).
fn spawn_checkpoint_thread(shared: &Arc<Shared>) -> Option<std::thread::JoinHandle<()>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = shared;
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("graphersal-checkpoint".into())
            .spawn(move || checkpoint_loop(&worker))
            .ok()
    }
}

/// The checkpoint thread: waits for the WAL threshold (or the shutdown) and checkpoints.
///
/// Every WAL record written while the level (WAL bytes since the last snapshot) is at or above
/// the threshold wakes it (wakes coalesce: one pending wake at most). After a checkpoint that
/// WROTE a snapshot the level is checked again at once: commits that landed while it ran may
/// have brought it back above the threshold (a single commit can exceed it), and the next
/// checkpoint covers them without waiting for another commit. After a failure, or a checkpoint
/// that wrote nothing (a snapshot at the durable end exists already), the thread waits for the
/// next wake, i.e. the next commit: a persistently failing checkpoint is retried at most once
/// per commit and never spins on its own.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn checkpoint_loop(shared: &Shared) {
    let mut again = false;
    loop {
        if !again {
            let Ok(mut due) = shared.wal.wake.lock() else {
                return;
            };
            while !*due {
                due = match shared.wal.wake_signal.wait(due) {
                    Ok(due) => due,
                    Err(_) => return,
                };
            }
            *due = false;
        }
        again = false;
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        let limit = shared.wal.checkpoint_after.load(Ordering::Acquire);
        let is_due = |shared: &Shared| {
            limit > 0
                && shared.wal.frozen().is_none()
                && !shared.wal.closed.load(Ordering::Acquire)
                && shared.wal.since_checkpoint.load(Ordering::Acquire) >= limit
        };
        if is_due(shared) {
            let result = shared.checkpoint(None, false);
            if let Err(err) = &result {
                tracing::warn!("the automatic checkpoint of {} failed: {err}", shared.dir);
                shared.report_checkpoint_damage(err);
            }
            again = result.as_ref().is_ok_and(|report| report.written) && is_due(shared);
            *shared
                .last_auto_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = result.err().map(Arc::new);
        }
    }
}

/// Whether a load failure is damage (maintenance mode) rather than a refusal (a version, a
/// memory limit, a lock).
fn is_damage(err: &PersistError) -> bool {
    match err {
        PersistError::Corrupt { .. }
        | PersistError::Gap { .. }
        | PersistError::LineageMismatch { .. } => true,
        PersistError::Io { source, .. } => matches!(
            source.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::InvalidData
                | std::io::ErrorKind::UnexpectedEof
        ),
        PersistError::NotAStore { reason, .. } => reason.contains("no snapshot"),
        PersistError::Graph(_) => true,
        _ => false,
    }
}

/// A damage error as a verification problem (the path relative to the store when the error
/// names a file of it).
fn problem_of(dir: &DirPath, err: &PersistError) -> VerifyProblem {
    let root = dir.display();
    let relative = |location: &str| {
        location
            .strip_prefix(&root)
            .map(|rest| rest.trim_start_matches('/').to_string())
            .unwrap_or_else(|| location.to_string())
    };
    match err {
        PersistError::Corrupt {
            location,
            offset,
            reason,
        } => VerifyProblem {
            path: relative(location),
            offset: Some(*offset),
            reason: reason.clone(),
        },
        other => VerifyProblem {
            path: ".".into(),
            offset: None,
            reason: other.to_string(),
        },
    }
}

/// Damage of one copy of the latest snapshot manifest's fixed part (the other copy was used).
/// A best-effort check after the load: a read that fails here is a warning, not a silence.
fn manifest_copy_warnings(dir: &DirPath) -> Vec<String> {
    let snapshots = match list_snapshots(dir) {
        Ok(snapshots) => snapshots,
        Err(err) => {
            return vec![format!(
                "the snapshots could not be listed again to check their manifest copies: {err}"
            )];
        }
    };
    let Some((seq, path)) = snapshots.last() else {
        return Vec::new();
    };
    let bytes = match files::read_bounded(&path.join(snapshot_dir::MANIFEST_FILE), 256 << 20) {
        Ok(bytes) => bytes,
        Err(err) => {
            return vec![format!(
                "snapshot {seq}: its manifest could not be read again to check its two fixed \
                 copies ({err}); run `graphersal store verify`"
            )];
        }
    };
    let fixed = super::manifest::FIXED_BYTES;
    if bytes.len() < 2 * fixed {
        return Vec::new();
    }
    let head = super::manifest::ManifestFixed::decode(&bytes[..fixed]);
    let tail = super::manifest::ManifestFixed::decode(&bytes[bytes.len() - fixed..]);
    match (head, tail) {
        (Ok(head), Ok(tail)) if head == tail => Vec::new(),
        _ => vec![format!(
            "snapshot {seq}: one copy of the manifest's fixed part is damaged (the other one was \
             used); run `graphersal store verify` and plan a repair"
        )],
    }
}

/// The commit hook of a store in maintenance mode: refuses every commit.
struct MaintenanceHook {
    message: String,
    damage: usize,
    path: String,
}

impl crate::graph::CommitHook for MaintenanceHook {
    fn name(&self) -> &str {
        "maintenance"
    }

    fn before_commit(
        &mut self,
        _: &crate::graph::ChangeSet,
    ) -> Result<(), crate::graph::HookError> {
        tracing::debug!("{}", self.message);
        Err(Box::new(PersistError::Maintenance {
            path: self.path.clone(),
            damage: self.damage,
        }))
    }
}

/// The commit hook of a store that holds something critical this version does not know
/// (format spec 16 rule 3): refuses every commit.
struct ReadOnlyHook {
    path: String,
    reason: String,
}

impl crate::graph::CommitHook for ReadOnlyHook {
    fn name(&self) -> &str {
        "read-only"
    }

    fn before_commit(
        &mut self,
        _: &crate::graph::ChangeSet,
    ) -> Result<(), crate::graph::HookError> {
        Err(Box::new(PersistError::ReadOnly {
            path: self.path.clone(),
            reason: self.reason.clone(),
        }))
    }
}

/// The commit hook of a backup opened read-only: refuses every commit.
struct BackupHook {
    path: String,
    commit_seq: u64,
}

impl crate::graph::CommitHook for BackupHook {
    fn name(&self) -> &str {
        "backup"
    }

    fn before_commit(
        &mut self,
        _: &crate::graph::ChangeSet,
    ) -> Result<(), crate::graph::HookError> {
        Err(Box::new(PersistError::IsBackup {
            path: self.path.clone(),
            commit_seq: self.commit_seq,
        }))
    }
}

/// The commit hook of every store opened read-write: refuses every commit once damage found
/// on disk while the store is open froze it (its damage policy `maintenance`).
struct DamageGuard {
    wal: Arc<WalShared>,
    path: String,
}

impl crate::graph::CommitHook for DamageGuard {
    fn name(&self) -> &str {
        "damage"
    }

    fn before_commit(
        &mut self,
        _: &crate::graph::ChangeSet,
    ) -> Result<(), crate::graph::HookError> {
        match self.wal.frozen() {
            Some((found_by, problems)) => Err(Box::new(PersistError::DamageFound {
                path: self.path.clone(),
                found_by,
                problems,
            })),
            None => Ok(()),
        }
    }
}

/// What loading a store's files into its graph produced (the graph holds the state).
struct LoadedFiles {
    /// Every WAL segment of the store, in order.
    segments: Vec<(u64, DirPath)>,
    /// The segments replayed onto the snapshot.
    replay: Vec<(u64, DirPath)>,
    /// The marks read from the replayed WAL.
    marks: Vec<Mark>,
    /// Why the state must not be written by this version (format spec 16 rule 3).
    read_only: Option<String>,
}

/// Loads the latest snapshot and replays the WAL after it (format spec 12.1, 12.2) into `graph`
/// (cleared first: its hooks stay), cutting an interrupted last write; fills the load part of
/// `report`.
fn load_latest(
    dir: &DirPath,
    options: &StoreOptions,
    graph_file: &GraphFile,
    report: &mut OpenReport,
    graph: &mut dyn LiveWrite,
) -> Result<LoadedFiles, PersistError> {
    let snapshots = list_snapshots(dir)?;
    let Some((snapshot_seq, snapshot_path)) = snapshots.last().cloned() else {
        return Err(PersistError::NotAStore {
            path: dir.display(),
            reason: "it has no snapshot".into(),
        });
    };
    let mut segments = list_wal(dir)?;
    // The set of segments against GRAPH (format spec 9.8): a missing, emptied or replaced
    // segment is damage, never a silently shorter history.
    let problems = wal_set_problems(&segments, graph_file);
    if let Some((name, reason)) = problems.first() {
        return Err(PersistError::corrupt(
            dir.join(name).display(),
            0,
            reason.clone(),
        ));
    }
    // A last segment shorter than its header, newer than the one GRAPH records as started: a
    // crash while it was being started (its header is written and synced before GRAPH records
    // it and before any record goes into it). It holds nothing.
    if let Some((first, path)) = segments.last().cloned()
        && graph_file.wal_head.is_none_or(|head| first > head)
    {
        let len = path
            .len()
            .map_err(|err| PersistError::io("reading the last WAL segment", err))?;
        if len < super::wal::WAL_HEADER_BYTES as u64 {
            remove_path(&path)
                .map_err(|err| PersistError::io("removing a torn WAL segment", err))?;
            files::sync_dir(&dir.join(WAL_DIR))?;
            report.torn_tail = Some(CutTail {
                segment: first,
                offset: 0,
                bytes: len,
            });
            segments.pop();
        }
    }
    let replay = wal_after(&segments, snapshot_seq).to_vec();
    // A graph schema a newer version wrote is shown as no schema and the store opens read-only
    // (format spec 16): only this load is lenient, never a load whose state is written.
    let newer = super::snapshot::NewerContent::default();
    let mut read = options.read.clone();
    read.newer_schema = Some(newer.clone());
    let plan = LoadPlan {
        snapshot: snapshot_path.clone(),
        journals: replay
            .iter()
            .map(|(_, path)| (path.clone(), None))
            .collect(),
        target: RecoveryTarget::Latest,
        read,
        chain: graph_file.chain(),
    };
    let recovered = graph.reload(&plan)?;
    if graph.graph_id() != graph_file.graph_id {
        return Err(PersistError::corrupt(
            snapshot_path.display(),
            16,
            "the snapshot and the WAL end in another lineage than the store's GRAPH file",
        ));
    }
    // A clean close recorded its last commit: the files must still hold it.
    if graph_file.clean
        && let Some(closed_at) = graph_file.closed_at
        && recovered.commit_seq < closed_at
    {
        return Err(PersistError::corrupt(
            dir.join(WAL_DIR).display(),
            0,
            format!(
                "the store was closed cleanly at commit {closed_at}, but its files end at commit \
                 {}: the WAL after it is lost",
                recovered.commit_seq
            ),
        ));
    }
    report.snapshot_commit_seq = recovered.snapshot_commit_seq;
    report.commit_seq = recovered.commit_seq;
    report.commits_replayed = recovered.commits_replayed;
    // Something critical this version does not know (format spec 16 rule 3): read-only, so
    // nothing is written from here on, not even the torn-tail cut.
    // The manifest was read by the load just now: a failure here is a real one (an I/O error,
    // or damage that opens the store in maintenance mode), never "nothing critical".
    let read_only = match graph.critical_definition()?.or_else(|| newer.first()) {
        Some(reason) => Some(reason),
        None => read_manifest(&snapshot_path)?.1.critical_segment_reason(),
    };
    if read_only.is_some() {
        return Ok(LoadedFiles {
            segments,
            replay,
            marks: recovered.marks,
            read_only,
        });
    }
    if let Some(torn) = recovered.torn_tail
        && let Some((first, path)) = replay.get(torn.journal)
    {
        path.truncate(torn.offset)
            .map_err(|err| PersistError::io("cutting the torn WAL tail", err))?;
        report.torn_tail = Some(CutTail {
            segment: *first,
            offset: torn.offset,
            bytes: torn.bytes,
        });
    }
    Ok(LoadedFiles {
        segments,
        replay,
        marks: recovered.marks,
        read_only: None,
    })
}

/// The derived `marks` file, completed with marks whose WAL record is durable but whose file
/// append was interrupted (rebuilt from the WAL when missing or damaged).
fn sync_marks(
    dir: &DirPath,
    loaded: &LoadedFiles,
    options: &StoreOptions,
    report: &mut OpenReport,
) -> Result<Vec<Mark>, PersistError> {
    let max_depth = options.read.max_value_depth();
    let mut marks = match read_marks(dir)? {
        Some(marks) => marks,
        None => {
            report.marks_rebuilt = true;
            let marks = scan_marks(&loaded.segments, max_depth);
            write_marks(dir, &marks)?;
            marks
        }
    };
    for mark in &loaded.marks {
        if !marks.iter().any(|m| m.name() == mark.name()) {
            append_mark(dir, mark)
                .map_err(|err| PersistError::io("writing the marks file", err))?;
            marks.push(mark.clone());
        }
    }
    Ok(marks)
}

/// Puts a new journal for `graph` into the Store's journal cell (the old one, if any, is
/// dropped): it continues the last WAL segment of the graph's lineage, or starts one, and
/// publishes its durable end through `durable`. The graph's journal slot is attached once, at
/// the open ([`LiveWrite::attach_journal`] of [`JournalCell::slot`]).
#[allow(clippy::too_many_arguments)]
fn attach_journal(
    dir: &DirPath,
    graph: &mut dyn LiveWrite,
    cell: &JournalCell,
    options: &StoreOptions,
    wal: &Arc<WalShared>,
    graph_file: &Arc<Mutex<GraphFile>>,
    segments: &[(u64, DirPath)],
    replay: &[(u64, DirPath)],
    marks: &[Mark],
    durable: DurableEnd,
) -> Result<DurableEnd, PersistError> {
    let since: u64 = replay
        .iter()
        .filter_map(|(_, path)| path.len().ok())
        .map(|len| len.saturating_sub(super::wal::WAL_HEADER_BYTES as u64))
        .sum();
    wal.since_checkpoint.store(since, Ordering::Release);
    let graph_id = graph.graph_id();
    let next = graph.last_commit_seq()?.checked_add(1).ok_or(
        crate::error::GraphError::SequenceOverflow {
            sequence: crate::error::COMMIT_SEQUENCE,
        },
    )?;
    let last_lineage = match segments.last() {
        Some((_, path)) => segment_lineage(path)?,
        None => None,
    };
    // A segment of an older file format version (a store upgraded from it, format spec 15.1)
    // is never continued: a record of the current version, which may hold value tags that
    // version does not have, goes into a new segment of the current version (17, writers).
    let continue_last = match segments.last() {
        Some((first, path)) if last_lineage == Some((graph_id, super::codec::FORMAT_VERSION)) => {
            let len = path
                .len()
                .map_err(|err| PersistError::io("opening the last WAL segment", err))?;
            Some((*first, path.clone(), len))
        }
        Some((first, path)) if last_lineage.is_some_and(|(id, _)| id == graph_id) => {
            let len = path
                .len()
                .map_err(|err| PersistError::io("opening the last WAL segment", err))?;
            if *first == next {
                // It holds no record (it starts at the next commit): replaced whole by a header
                // of the current version under the same name, atomically.
                if len > super::wal::WAL_HEADER_BYTES as u64 {
                    return Err(PersistError::corrupt(
                        path.display(),
                        super::wal::WAL_HEADER_BYTES as u64,
                        format!("it starts at commit {next}, which is not committed yet"),
                    ));
                }
                let header = super::wal::WalHeader {
                    graph_id,
                    first_commit_seq: next,
                    created_at: crate::graph::undo::now_micros(),
                };
                path.write_atomic(&header.encode())
                    .map_err(|err| PersistError::io("starting a WAL segment", err))?;
                Some((*first, path.clone(), super::wal::WAL_HEADER_BYTES as u64))
            } else {
                None
            }
        }
        _ => None,
    };
    let (file, segment, offset) = match continue_last {
        Some((first, path, len)) => {
            // Written to GRAPH with the open's next write of it.
            if let Ok(mut file) = graph_file.lock() {
                file.wal_head = Some(first);
            }
            (
                open_segment_at(&path, len)
                    .map_err(|err| PersistError::io("opening the last WAL segment", err))?,
                first,
                len,
            )
        }
        None => {
            let file = create_segment(dir, graph_id, next)
                .map_err(|err| PersistError::io("starting a WAL segment", err))?;
            record_wal_head(dir, graph_file, next);
            (file, next, super::wal::WAL_HEADER_BYTES as u64)
        }
    };
    let files = StoreWalFiles {
        dir: dir.clone(),
        graph_id,
        segment_bytes: options.wal_segment_bytes,
        current: segment,
        shared: wal.clone(),
        broken: false,
        graph_file: graph_file.clone(),
        faults: options.faults.clone(),
    };
    let faults = options.faults.clone();
    let (journal, durable) = Journal::new(WalWriter { file }, options.durability)
        .with_sync(move |writer: &mut WalWriter| {
            if let Some(faults) = &faults {
                faults.sync()?;
            }
            writer.file.sync()
        })
        .with_codec(options.codec)
        .with_known_marks(marks.iter().map(|m| m.name().to_string()))
        .with_files(Box::new(files))
        .with_durable_end(durable)
        .continuing(graph.last_commit_seq()?, segment, offset);
    cell.set(Some(journal));
    Ok(durable)
}

/// How much of WAL segment `first` is valid given a recorded cut of a refused record.
pub(super) fn cut_limit(cut: Option<(u64, u64)>, first: u64) -> Option<u64> {
    match cut {
        Some((segment, offset)) if segment == first => Some(offset),
        _ => None,
    }
}

/// Leaves out the WAL segments after a recorded cut (they can only hold refused bytes).
pub(super) fn drop_refused_segments(segments: &mut Vec<(u64, DirPath)>, cut: Option<(u64, u64)>) {
    if let Some((segment, _)) = cut {
        segments.retain(|(first, _)| *first <= segment);
    }
}

/// Cuts a refused record that could not be cut when it was refused (format spec 9.7): the
/// segment back to the recorded offset, later segments removed.
fn apply_wal_cut(
    dir: &DirPath,
    (segment, offset): (u64, u64),
) -> Result<Option<CutTail>, PersistError> {
    let mut cut = None;
    for (first, path) in list_wal(dir)? {
        if first == segment {
            let len = path
                .len()
                .map_err(|err| PersistError::io("reading a WAL segment", err))?;
            if len > offset {
                path.truncate(offset)
                    .map_err(|err| PersistError::io("cutting a refused WAL record", err))?;
                cut = Some(CutTail {
                    segment,
                    offset,
                    bytes: len - offset,
                });
            }
        } else if first > segment {
            remove_path(&path)
                .map_err(|err| PersistError::io("removing a refused WAL segment", err))?;
        }
    }
    files::sync_dir(&dir.join(WAL_DIR))?;
    Ok(cut)
}

/// Copies the tree below `at` (a name of `from`) into `to` ([`Store::convert`]): directories
/// first, files with their bytes (synced); `GRAPH` and `GRAPH.copy` are collected for last,
/// the locks and temporary files are left out.
fn copy_tree(
    from: &DirPath,
    to: &DirPath,
    at: &DirPath,
    copied: &mut (usize, u64),
    graph_files: &mut Vec<String>,
) -> Result<(), PersistError> {
    let io = |err| PersistError::io("copying the store", err);
    let relative = at.relative_to(from);
    to.join(&relative).create_dir_all().map_err(io)?;
    let mut entries = at.list().map_err(io)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, kind) in entries {
        let source = at.join(&name);
        let path = source.relative_to(from);
        if kind == files::EntryKind::Dir {
            if !name.starts_with(TMP_PREFIX) {
                copy_tree(from, to, &source, copied, graph_files)?;
            }
            continue;
        }
        if relative.is_empty() {
            match name.as_str() {
                files::GRAPH_FILE | files::GRAPH_COPY => {
                    graph_files.push(name);
                    continue;
                }
                "LOCK" | "BACKUP" => continue,
                _ if name.ends_with(".tmp") => continue,
                _ => {}
            }
        }
        let len = source.len().map_err(io)?;
        let mut out = to.join(&path).create(true).map_err(io)?;
        maintenance::copy_exact(&source, len, &mut out)?;
        out.sync().map_err(io)?;
        copied.0 += 1;
        copied.1 += len;
    }
    files::sync_dir(&to.join(&relative))
}

/// Refuses a newer format version (an older one never decodes: `read_graph_file` refuses it
/// with [`PersistError::OlderFormat`]).
fn guard_version(dir: &DirPath, version: u32) -> Result<(), PersistError> {
    if version > STORE_VERSION {
        return Err(PersistError::UnsupportedVersion {
            location: format!("store {}", dir.display()),
            found: version,
            supported: STORE_VERSION,
        });
    }
    Ok(())
}

/// Completes an interrupted multi-file operation (idempotent steps).
fn complete_intent(dir: &DirPath, intent: &Intent) -> Result<(), PersistError> {
    match intent.op.as_str() {
        "prune" => {
            for relative in &intent.files {
                let path = safe_relative(dir, relative).ok_or_else(|| {
                    PersistError::corrupt(
                        files::INTENT_FILE,
                        0,
                        format!("unsafe path {relative:?}"),
                    )
                })?;
                remove_path(&path).map_err(|err| PersistError::io("completing a prune", err))?;
            }
            files::sync_dir(&dir.join(SNAPSHOTS_DIR))?;
            files::sync_dir(&dir.join(WAL_DIR))?;
        }
        // Both clear the intent themselves, as their last step.
        "rollback" => return rollback::apply_rollback(dir, intent, None),
        "attic_restore" => return rollback::apply_restore(dir, intent, None),
        // Clears the intent itself.
        backup::INCREMENT_OP => return backup::undo_increment(dir, intent),
        // An operation of a newer version: it cannot be completed here, and it is not damage
        // (format spec 16); the store is left as it is.
        other => {
            return Err(PersistError::unsupported_content(
                files::INTENT_FILE,
                0,
                format!(
                    "an interrupted operation {other:?} this version does not know (written by \
                     a newer Graphersal?); complete it with that version"
                ),
            ));
        }
    }
    Intent::clear(dir)
}

/// Removes `snapshots/tmp-*` and stray `*.tmp` files; returns how many.
fn remove_tmp(dir: &DirPath) -> Result<usize, PersistError> {
    let mut removed = 0;
    let snapshots_dir = dir.join(SNAPSHOTS_DIR);
    let snapshots = snapshots_dir
        .list()
        .map_err(|err| PersistError::io("listing snapshots", err))?;
    for (name, _) in snapshots {
        if name.starts_with(TMP_PREFIX) {
            remove_path(&snapshots_dir.join(&name))
                .map_err(|err| PersistError::io("removing a temporary snapshot", err))?;
            removed += 1;
        }
    }
    let root = dir
        .list()
        .map_err(|err| PersistError::io("listing the store", err))?;
    for (name, kind) in root {
        if name.ends_with(".tmp") && kind == files::EntryKind::File {
            remove_path(&dir.join(&name))
                .map_err(|err| PersistError::io("removing a temporary file", err))?;
            removed += 1;
        }
    }
    removed += rollback::remove_interrupted(dir);
    removed += models::remove_tmp(dir);
    if removed > 0 {
        files::sync_dir(&dir.join(SNAPSHOTS_DIR))?;
        files::sync_dir(dir)?;
    }
    Ok(removed)
}

/// The lineage id in a WAL segment's header (`None`: no valid header). A failed read is an
/// error, never "another lineage": the caller would otherwise start a new segment over it and
/// report that follow-on failure instead of the read error.
/// With the lineage, the file format version of the header (format spec 9.1, 15).
fn segment_lineage(path: &DirPath) -> Result<Option<(u128, u32)>, PersistError> {
    let source = open_segment_reader(path, Some(super::wal::WAL_HEADER_BYTES as u64))?;
    match super::wal::JournalReader::open(source, 0, 1) {
        Ok(reader) => Ok(Some((reader.graph_id(), reader.format_version()))),
        Err(err @ PersistError::Io { .. }) => Err(err),
        Err(_) => Ok(None),
    }
}

fn list_snapshot_infos(dir: &DirPath) -> Result<Vec<SnapshotInfo>, PersistError> {
    list_snapshots(dir)?
        .iter()
        .map(|(_, path)| read_listing(path))
        .collect()
}

/// The marks a target can reach (format spec 10): the `marks` file (a WAL scan when it is
/// missing) without the marks before the oldest snapshot, whose history a prune removed.
fn list_marks(dir: &DirPath) -> Result<Vec<Mark>, PersistError> {
    reachable_marks(dir, all_marks(dir)?)
}

/// The marks a target can reach at or before `up_to`, for the reports of a fork or repair that
/// does not carry them. A fork computes them before it writes anything and fails when they
/// cannot be read; a repair (of a damaged store) reports the failure in
/// [`RepairReport::marks_unreadable`] instead.
fn marks_up_to(dir: &DirPath, up_to: u64) -> Result<Vec<Mark>, PersistError> {
    let mut marks = list_marks(dir)?;
    marks.retain(|mark| mark.commit_seq() <= up_to);
    Ok(marks)
}

/// The guard of `lock`, or [`StoreFailure::LockPoisoned`] naming `operation` when a thread
/// panicked while it held it (what it guards may be half-changed: never answered as a value).
fn poisoned_or<'a, T>(
    lock: &'a Mutex<T>,
    operation: &'static str,
) -> Result<std::sync::MutexGuard<'a, T>, PersistError> {
    lock.lock().map_err(|_| {
        PersistError::store(
            operation,
            StoreFailure::LockPoisoned,
            "a store lock is poisoned: a thread panicked while it held it",
        )
    })
}

/// Every mark of the `marks` file (a WAL scan when it is missing), reachable or not.
fn all_marks(dir: &DirPath) -> Result<Vec<Mark>, PersistError> {
    Ok(match read_marks(dir)? {
        Some(marks) => marks,
        None => scan_marks(
            &list_wal(dir)?,
            crate::value::depth::DEFAULT_MAX_VALUE_DEPTH,
        ),
    })
}

fn info_of(dir: &DirPath, file: &GraphFile) -> Result<StoreInfo, PersistError> {
    let segments = list_wal(dir)?;
    let wal_bytes = segments
        .iter()
        .filter_map(|(_, path)| path.len().ok())
        .sum();
    Ok(StoreInfo {
        location: dir.display(),
        format_version: file.version,
        graph_id: file.graph_id,
        lineage: file.lineage.clone(),
        created_at: file.created_at,
        chunk_bytes: file.chunk_bytes,
        commit_seq: 0,
        last_commit_at: 0,
        durable: None,
        snapshots: list_snapshot_infos(dir)?,
        marks: list_marks(dir)?,
        wal_segments: segments.len(),
        wal_bytes,
        clean: file.clean,
        attic: rollback::attic_list(dir)?,
        snapshot_bytes: rollback::tree_bytes(&dir.join(SNAPSHOTS_DIR)),
        backup: file.backup,
        space: dir
            .store_dir()
            .space_usage()
            .map_err(|err| PersistError::io("reading the space usage of the store", err))?,
        definitions: 0,
        definitions_by_kind: Default::default(),
        damage_policy: file.damage_policy,
        store_id: file.store_id,
        damage_found: Vec::new(),
        frozen_by_damage: false,
        stopped: None,
        models: models::list(dir)?,
        schema_models: Vec::new(),
    })
}

fn export_packed_from(
    dir: &DirPath,
    commit_seq: Option<u64>,
    mut writer: impl std::io::Write,
    models: ModelCopy,
) -> Result<SnapshotInfo, PersistError> {
    let snapshots = list_snapshots(dir)?;
    let found = match commit_seq {
        None => snapshots.last(),
        Some(seq) => snapshots.iter().find(|(s, _)| *s == seq),
    };
    let Some((_, path)) = found else {
        return Err(PersistError::store(
            "export the snapshot",
            StoreFailure::NoSnapshot,
            match commit_seq {
                Some(seq) => format!("the store has no snapshot at commit {seq}"),
                None => "the store has no snapshot".into(),
            },
        ));
    };
    let model_files = match models {
        ModelCopy::Include => models::packed_files(dir)?.0,
        ModelCopy::Exclude => Vec::new(),
    };
    let mut reader = DirSnapshotReader::open_with_models(path, model_files)?;
    std::io::copy(&mut reader, &mut writer)
        .map_err(|err| PersistError::io("exporting a packed snapshot", err))?;
    writer
        .flush()
        .map_err(|err| PersistError::io("exporting a packed snapshot", err))?;
    read_listing(path)
}

/// [`Store::remove_model`] in the store at `dir` (its writer holds the lock): refused while a
/// backup copies the store's files.
fn remove_model_at(dir: &DirPath, key: &str) -> Result<StoredModel, PersistError> {
    let _pin = match dir.store_dir().lock(super::dir::LockKind::BackupExclusive) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Err(PersistError::store(
                "remove a model",
                StoreFailure::BackupRunning,
                "a backup is copying the store's files; remove the model when it has finished",
            ));
        }
        Err(err) => return Err(PersistError::io("opening the store's BACKUP file", err)),
    };
    models::remove(dir, key)
}

/// [`Store::read_model_file`] in the store at `dir`.
fn read_model_file_at(dir: &DirPath, key: &str, name: &str) -> Result<Vec<u8>, PersistError> {
    let Some(model) = models::find(dir, key)? else {
        return Err(models::unknown_model("read a model file", key));
    };
    models::read_file(dir, &model.id, name)
}

impl Store {
    /// [`Store::export_packed`] of the store in `dir`, without a lock.
    pub fn export_packed_dir(
        dir: impl IntoStoreDir,
        commit_seq: Option<u64>,
        writer: impl std::io::Write,
    ) -> Result<SnapshotInfo, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_older_format(&dir)?;
        export_packed_from(&dir, commit_seq, writer, ModelCopy::Include)
    }

    /// [`Store::export_packed_with`] of the store in `dir`, without a lock.
    pub fn export_packed_dir_with(
        dir: impl IntoStoreDir,
        commit_seq: Option<u64>,
        writer: impl std::io::Write,
        models: ModelCopy,
    ) -> Result<SnapshotInfo, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_older_format(&dir)?;
        export_packed_from(&dir, commit_seq, writer, models)
    }

    /// [`Store::models`] of the store in `dir`, without a lock.
    pub fn models_dir(dir: impl IntoStoreDir) -> Result<Vec<StoredModel>, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_other_version(&dir)?;
        read_graph_file(&dir)?;
        models::list(&dir)
    }

    /// [`Store::add_model`] to the CLOSED store in `dir` (CLI:
    /// `graphersal embed-model download <repo> --into <dir>`): takes the writer lock (a store open elsewhere is refused with
    /// [`PersistError::Locked`]); refused on a backup.
    pub fn add_model_dir(
        dir: impl IntoStoreDir,
        model: NewModel,
    ) -> Result<AddModelReport, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        let _lock = Self::closed_store_lock(&dir)?;
        let (model, added) = models::add(&dir, model, models::now_text())?;
        Ok(AddModelReport {
            model,
            already_stored: !added,
        })
    }

    /// [`Store::remove_model`] from the CLOSED store in `dir` (CLI:
    /// `graphersal store models remove <dir> <identity>`): takes the writer lock; refused on a backup and while a backup copies
    /// the store's files.
    pub fn remove_model_dir(
        dir: impl IntoStoreDir,
        key: &str,
    ) -> Result<StoredModel, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        let _lock = Self::closed_store_lock(&dir)?;
        remove_model_at(&dir, key)
    }

    /// [`Store::read_model_file`] of the store in `dir`, without a lock.
    pub fn read_model_file_dir(
        dir: impl IntoStoreDir,
        key: &str,
        name: &str,
    ) -> Result<Vec<u8>, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_other_version(&dir)?;
        read_model_file_at(&dir, key, name)
    }

    /// The writer lock of a closed store that is no backup (a model add or removal).
    fn closed_store_lock(dir: &DirPath) -> Result<DirLock, PersistError> {
        files::refuse_other_version(dir)?;
        if !Self::is_store(dir.store_dir())? {
            return Err(PersistError::NotAStore {
                path: dir.display(),
                reason: "it has no GRAPH file".into(),
            });
        }
        let lock = DirLock::exclusive(dir)?;
        if let Some(marker) = read_graph_file(dir)?.file.backup {
            return Err(PersistError::IsBackup {
                path: dir.display(),
                commit_seq: marker.commit_seq,
            });
        }
        Ok(lock)
    }

    /// The snapshots of the store in `dir`, without a lock.
    pub fn snapshots_dir(dir: impl IntoStoreDir) -> Result<Vec<SnapshotInfo>, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_older_format(&dir)?;
        list_snapshot_infos(&dir)
    }

    /// The marks of the store in `dir` a target can reach (see [`Store::marks`]), without a
    /// lock.
    pub fn marks_dir(dir: impl IntoStoreDir) -> Result<Vec<Mark>, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        files::refuse_older_format(&dir)?;
        list_marks(&dir)
    }

    /// Whether `dir` holds a store (a `GRAPH` or `GRAPH.copy` file, or a single-file store of a
    /// newer container version, which every operation then refuses as newer). A missing
    /// directory is `Ok(false)`; a directory or file that cannot be read is
    /// [`PersistError::Io`] (never "not a store").
    pub fn is_store(dir: impl IntoStoreDir) -> Result<bool, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        // A single-file store of a newer container version is a store, which this build does
        // not read: the operation refuses it as newer (format spec 15), never "not a store".
        if files::refuse_newer_container(&dir).is_err() {
            return Ok(true);
        }
        let kind = |at: &DirPath| {
            at.try_kind()
                .map_err(|err| PersistError::io("read a store directory", err))
        };
        Ok(kind(&dir)? == Some(files::EntryKind::Dir)
            && (kind(&dir.join(files::GRAPH_FILE))? == Some(files::EntryKind::File)
                || kind(&dir.join(files::GRAPH_COPY))? == Some(files::EntryKind::File)))
    }
}

#[cfg(test)]
mod backup_check_tests;
#[cfg(test)]
mod merge_tests;
#[cfg(test)]
mod tests;

/// Counts definitions per kind, keyed by the kind's display name (`query`, `compression`,
/// `kind <n>`).
pub(crate) fn count_by_kind(
    kinds: impl Iterator<Item = crate::graph::DefinitionKind>,
) -> std::collections::BTreeMap<String, u64> {
    let mut counts = std::collections::BTreeMap::new();
    for kind in kinds {
        *counts.entry(kind.to_string()).or_insert(0) += 1;
    }
    counts
}
